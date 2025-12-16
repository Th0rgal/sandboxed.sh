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
  const [saved, setSaved] = useState(false);

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
    setSaved(true);
    setTimeout(() => setSaved(false), 2000);
  };

  return (
    <div className="min-h-screen flex flex-col items-center p-6">
      {/* Centered content container */}
      <div className="w-full max-w-xl">
        {/* Header */}
        <div className="mb-8">
          <h1 className="text-xl font-semibold text-white">Settings</h1>
          <p className="mt-1 text-sm text-white/50">Configure your server connection and preferences</p>
        </div>

        <div className="space-y-5">
          {/* Connection Status */}
          <div className="rounded-xl bg-white/[0.02] border border-white/[0.04] p-5">
            <div className="flex items-center gap-3 mb-4">
              <div className="flex h-10 w-10 items-center justify-center rounded-xl bg-indigo-500/10">
                <Server className="h-5 w-5 text-indigo-400" />
              </div>
              <div>
                <h2 className="text-sm font-medium text-white">API Connection</h2>
                <p className="text-xs text-white/40">Configure server endpoint</p>
              </div>
            </div>

            <div className="space-y-4">
              <div>
                <label className="block text-xs font-medium text-white/60 mb-1.5">
                  API URL
                </label>
                <input
                  type="text"
                  value={apiUrl}
                  onChange={(e) => setApiUrl(e.target.value)}
                  className="w-full rounded-lg border border-white/[0.06] bg-white/[0.02] px-3 py-2.5 text-sm text-white placeholder-white/30 focus:border-indigo-500/50 focus:outline-none transition-colors"
                />
              </div>

              <div className="flex items-center gap-2">
                <span className="text-xs text-white/40">Status:</span>
                {health ? (
                  <span className="flex items-center gap-1.5 text-xs text-emerald-400">
                    <span className="h-1.5 w-1.5 rounded-full bg-emerald-400" />
                    Connected (v{health.version})
                  </span>
                ) : (
                  <span className="flex items-center gap-1.5 text-xs text-red-400">
                    <span className="h-1.5 w-1.5 rounded-full bg-red-400" />
                    Disconnected
                  </span>
                )}
              </div>
            </div>
          </div>

          {/* Default Model */}
          <div className="rounded-xl bg-white/[0.02] border border-white/[0.04] p-5">
            <div className="flex items-center gap-3 mb-4">
              <div className="flex h-10 w-10 items-center justify-center rounded-xl bg-indigo-500/10">
                <Cpu className="h-5 w-5 text-indigo-400" />
              </div>
              <div>
                <h2 className="text-sm font-medium text-white">Default Model</h2>
                <p className="text-xs text-white/40">Choose the AI model for tasks</p>
              </div>
            </div>

            <div>
              <label className="block text-xs font-medium text-white/60 mb-1.5">
                Model ID
              </label>
              <select
                value={defaultModel}
                onChange={(e) => setDefaultModel(e.target.value)}
                className="w-full rounded-lg border border-white/[0.06] bg-white/[0.02] px-3 py-2.5 text-sm text-white focus:border-indigo-500/50 focus:outline-none transition-colors"
              >
                <option value="anthropic/claude-sonnet-4.5">Claude Sonnet 4.5 (Recommended)</option>
                <option value="anthropic/claude-haiku-4.5">Claude Haiku 4.5 (Budget)</option>
                <option value="anthropic/claude-opus-4.5">Claude Opus 4.5 (Premium)</option>
                <option value="openai/gpt-4o">GPT-4o</option>
                <option value="openai/gpt-4o-mini">GPT-4o Mini</option>
              </select>
            </div>
          </div>

          {/* Budget */}
          <div className="rounded-xl bg-white/[0.02] border border-white/[0.04] p-5">
            <div className="flex items-center gap-3 mb-4">
              <div className="flex h-10 w-10 items-center justify-center rounded-xl bg-indigo-500/10">
                <Wallet className="h-5 w-5 text-indigo-400" />
              </div>
              <div>
                <h2 className="text-sm font-medium text-white">Default Budget</h2>
                <p className="text-xs text-white/40">Maximum spend per task</p>
              </div>
            </div>

            <div>
              <label className="block text-xs font-medium text-white/60 mb-1.5">
                Budget per task (cents)
              </label>
              <input
                type="number"
                value={defaultBudget}
                onChange={(e) => setDefaultBudget(e.target.value)}
                className="w-full rounded-lg border border-white/[0.06] bg-white/[0.02] px-3 py-2.5 text-sm text-white placeholder-white/30 focus:border-indigo-500/50 focus:outline-none transition-colors"
              />
              <p className="mt-1.5 text-xs text-white/30">
                1000 cents = $10.00
              </p>
            </div>
          </div>

          {/* About */}
          <div className="rounded-xl bg-white/[0.02] border border-white/[0.04] p-5">
            <div className="flex items-center gap-3 mb-4">
              <div className="flex h-10 w-10 items-center justify-center rounded-xl bg-indigo-500/10">
                <Bot className="h-5 w-5 text-indigo-400" />
              </div>
              <div>
                <h2 className="text-sm font-medium text-white">About OpenAgent</h2>
                <p className="text-xs text-white/40">Autonomous coding agent</p>
              </div>
            </div>

            <div className="grid grid-cols-2 gap-2">
              <div className="flex items-center gap-2 rounded-lg bg-white/[0.02] px-3 py-2">
                <span className="h-1.5 w-1.5 rounded-full bg-indigo-400" />
                <span className="text-xs text-white/60">AI-maintainable</span>
              </div>
              <div className="flex items-center gap-2 rounded-lg bg-white/[0.02] px-3 py-2">
                <span className="h-1.5 w-1.5 rounded-full bg-emerald-400" />
                <span className="text-xs text-white/60">Self-contained</span>
              </div>
              <div className="flex items-center gap-2 rounded-lg bg-white/[0.02] px-3 py-2">
                <span className="h-1.5 w-1.5 rounded-full bg-amber-400" />
                <span className="text-xs text-white/60">Full-access</span>
              </div>
              <div className="flex items-center gap-2 rounded-lg bg-white/[0.02] px-3 py-2">
                <span className="h-1.5 w-1.5 rounded-full bg-blue-400" />
                <span className="text-xs text-white/60">Hierarchical</span>
              </div>
            </div>
          </div>

          {/* Save Button */}
          <button
            onClick={handleSave}
            className="w-full flex items-center justify-center gap-2 rounded-lg bg-indigo-500 hover:bg-indigo-600 px-4 py-2.5 text-sm font-medium text-white transition-colors"
          >
            <Save className="h-4 w-4" />
            {saved ? 'Saved!' : 'Save Settings'}
          </button>
        </div>
      </div>
    </div>
  );
}
