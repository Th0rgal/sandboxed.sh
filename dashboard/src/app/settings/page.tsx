'use client';

import { useState, useEffect } from 'react';
import { getHealth, HealthResponse } from '@/lib/api';
import { Server, Bot, Cpu, Wallet, Save } from 'lucide-react';
import { readSavedSettings, writeSavedSettings } from '@/lib/settings';

export default function SettingsPage() {
  const [health, setHealth] = useState<HealthResponse | null>(null);
  const [apiUrl, setApiUrl] = useState(() => readSavedSettings().apiUrl ?? 'http://127.0.0.1:3000');
  const [defaultModel, setDefaultModel] = useState(
    () => readSavedSettings().defaultModel ?? 'anthropic/claude-sonnet-4.5'
  );
  const [defaultBudget, setDefaultBudget] = useState(() => readSavedSettings().defaultBudget ?? '1000');

  useEffect(() => {
    const checkHealth = async () => {
      try {
        const data = await getHealth();
        setHealth(data);
      } catch {
        setHealth(null);
      }
    };
    checkHealth();
  }, []);

  const handleSave = () => {
    writeSavedSettings({ apiUrl, defaultModel, defaultBudget });
    alert('Settings saved!');
  };

  return (
    <div className="p-8">
      {/* Header */}
      <div className="mb-8">
        <h1 className="text-2xl font-bold text-[var(--foreground)]">Settings</h1>
        <p className="text-sm text-[var(--foreground-muted)]">Configure the dashboard and agent</p>
      </div>

      <div className="max-w-2xl space-y-6">
        {/* Connection Status */}
        <div className="panel rounded-lg p-6">
          <div className="flex items-center gap-3 mb-4">
            <Server className="h-5 w-5 text-[var(--accent)]" />
            <h2 className="text-lg font-semibold text-[var(--foreground)]">API Connection</h2>
          </div>

          <div className="space-y-4">
            <div>
              <label className="block text-sm font-medium text-[var(--foreground-muted)] mb-1">
                API URL
              </label>
              <input
                type="text"
                value={apiUrl}
                onChange={(e) => setApiUrl(e.target.value)}
                className="w-full rounded-lg border border-[var(--border)] bg-[var(--background)] px-4 py-2 text-sm text-[var(--foreground)] focus:border-[var(--accent)] focus:outline-none focus-visible:!border-[var(--border)]"
              />
            </div>

            <div className="flex items-center gap-2">
              <span className="text-sm text-[var(--foreground-muted)]">Status:</span>
              {health ? (
                <span className="flex items-center gap-1.5 text-sm text-[var(--success)]">
                  <span className="h-2 w-2 rounded-full bg-[var(--success)]" />
                  Connected (v{health.version})
                </span>
              ) : (
                <span className="flex items-center gap-1.5 text-sm text-[var(--error)]">
                  <span className="h-2 w-2 rounded-full bg-[var(--error)]" />
                  Disconnected
                </span>
              )}
            </div>
          </div>
        </div>

        {/* Default Model */}
        <div className="panel rounded-lg p-6">
          <div className="flex items-center gap-3 mb-4">
            <Cpu className="h-5 w-5 text-[var(--accent)]" />
            <h2 className="text-lg font-semibold text-[var(--foreground)]">Default Model</h2>
          </div>

          <div>
            <label className="block text-sm font-medium text-[var(--foreground-muted)] mb-1">
              Model ID
            </label>
            <select
              value={defaultModel}
              onChange={(e) => setDefaultModel(e.target.value)}
              className="w-full rounded-lg border border-[var(--border)] bg-[var(--background)] px-4 py-2 text-sm text-[var(--foreground)] focus:border-[var(--accent)] focus:outline-none focus-visible:!border-[var(--border)]"
            >
              <option value="anthropic/claude-sonnet-4.5">Claude Sonnet 4.5 (Recommended)</option>
              <option value="anthropic/claude-haiku-4.5">Claude Haiku 4.5 (Budget)</option>
              <option value="anthropic/claude-opus-4.5">Claude Opus 4.5 (Premium)</option>
              <option value="openai/gpt-4o">GPT-4o</option>
              <option value="openai/gpt-4o-mini">GPT-4o Mini</option>
            </select>
            <p className="mt-1 text-xs text-[var(--foreground-muted)]">
              The model used when no specific model is selected for a task
            </p>
          </div>
        </div>

        {/* Budget */}
        <div className="panel rounded-lg p-6">
          <div className="flex items-center gap-3 mb-4">
            <Wallet className="h-5 w-5 text-[var(--accent)]" />
            <h2 className="text-lg font-semibold text-[var(--foreground)]">Default Budget</h2>
          </div>

          <div>
            <label className="block text-sm font-medium text-[var(--foreground-muted)] mb-1">
              Budget per task (cents)
            </label>
            <input
              type="number"
              value={defaultBudget}
              onChange={(e) => setDefaultBudget(e.target.value)}
              className="w-full rounded-lg border border-[var(--border)] bg-[var(--background)] px-4 py-2 text-sm text-[var(--foreground)] focus:border-[var(--accent)] focus:outline-none focus-visible:!border-[var(--border)]"
            />
            <p className="mt-1 text-xs text-[var(--foreground-muted)]">
              1000 cents = $10.00 — Maximum budget allocated per task
            </p>
          </div>
        </div>

        {/* About */}
        <div className="panel rounded-lg p-6">
          <div className="flex items-center gap-3 mb-4">
            <Bot className="h-5 w-5 text-[var(--accent)]" />
            <h2 className="text-lg font-semibold text-[var(--foreground)]">About OpenAgent</h2>
          </div>

          <div className="space-y-2 text-sm text-[var(--foreground-muted)]">
            <p>OpenAgent is a minimal autonomous coding agent implemented in Rust.</p>
            <p>• AI-maintainable: Rust&apos;s type system provides immediate feedback</p>
            <p>• Self-contained: No external dependencies beyond OpenRouter</p>
            <p>• Full-access: Complete access to filesystem, terminal, network</p>
            <p>• Hierarchical: Tree of specialized agents for complex tasks</p>
          </div>
        </div>

        {/* Save Button */}
        <button
          onClick={handleSave}
          className="flex items-center gap-2 rounded-lg bg-[var(--accent)] px-4 py-2 text-sm font-medium text-white transition-colors hover:bg-[var(--accent)]/90"
        >
          <Save className="h-4 w-4" />
          Save Settings
        </button>
      </div>
    </div>
  );
}

