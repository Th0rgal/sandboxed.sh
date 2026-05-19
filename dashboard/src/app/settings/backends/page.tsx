'use client';

import { useState, useEffect, useCallback } from 'react';
import useSWR from 'swr';
import { toast } from '@/components/toast';
import {
  listBackends,
  updateBackendConfig,
  getProviderForBackend,
  getHealth,
  getSettings,
  updateSettings,
  BackendProviderResponse,
} from '@/lib/api';
import { Server, Save, Loader, Check } from 'lucide-react';
import { cn } from '@/lib/utils';
import { getRuntimeApiBase, writeSavedSettings } from '@/lib/settings';
import { ServerConnectionCard } from '@/components/server-connection-card';
import { useBackendConfigs } from '@/lib/use-backend-configs';

const SETTINGS_BACKEND_IDS = ['opencode', 'claudecode', 'grok'] as const;

export default function BackendsPage() {
  const [activeBackendTab, setActiveBackendTab] = useState<'opencode' | 'claudecode' | 'grok'>('opencode');
  const [savingBackend, setSavingBackend] = useState(false);
  const [savingMissionLimit, setSavingMissionLimit] = useState(false);
  const [savingTaskLimit, setSavingTaskLimit] = useState(false);
  const [testingConnection, setTestingConnection] = useState(false);
  const [maxParallelMissionsValue, setMaxParallelMissionsValue] = useState('1');
  const [maxConcurrentTasksValue, setMaxConcurrentTasksValue] = useState('5');

  // Server connection state
  const [apiUrl, setApiUrl] = useState(() => getRuntimeApiBase());
  const [originalApiUrl, setOriginalApiUrl] = useState(() => getRuntimeApiBase());
  const [urlError, setUrlError] = useState<string | null>(null);

  const { data: health, isLoading: healthLoading, mutate: mutateHealth } = useSWR(
    'health',
    getHealth,
    { revalidateOnFocus: false }
  );
  const { data: serverSettings, mutate: mutateSettings } = useSWR(
    'settings',
    getSettings,
    { revalidateOnFocus: false }
  );

  const hasUnsavedUrlChanges = apiUrl !== originalApiUrl;

  const validateUrl = useCallback((url: string) => {
    if (!url.trim()) { setUrlError('API URL is required'); return false; }
    try { new URL(url); setUrlError(null); return true; } catch { setUrlError('Invalid URL format'); return false; }
  }, []);

  const testApiConnection = async () => {
    if (!validateUrl(apiUrl)) return;
    setTestingConnection(true);
    try { await mutateHealth(); toast.success('Connection successful!'); } catch { toast.error('Failed to connect to server'); } finally { setTestingConnection(false); }
  };

  const handleSaveUrl = useCallback(() => {
    if (!validateUrl(apiUrl)) return;
    const prev = originalApiUrl;
    writeSavedSettings({ apiUrl });
    setOriginalApiUrl(apiUrl);
    toast.success('API URL saved!');
    if (prev !== apiUrl) window.dispatchEvent(new CustomEvent('openagent:api:url-changed'));
  }, [apiUrl, originalApiUrl, validateUrl]);
  const [opencodeForm, setOpencodeForm] = useState({
    base_url: '',
    default_agent: '',
    permissive: false,
    enabled: true,
  });
  const [claudeForm, setClaudeForm] = useState({
    api_key: '',
    cli_path: '',
    api_key_configured: false,
    enabled: true,
  });
  const [grokForm, setGrokForm] = useState({
    cli_path: '',
    enabled: true,
  });

  // SWR: fetch backends
  const { data: backends = [] } = useSWR('backends', listBackends, {
    revalidateOnFocus: false,
    fallbackData: [
      { id: 'opencode', name: 'OpenCode' },
      { id: 'claudecode', name: 'Claude Code' },
      { id: 'grok', name: 'Grok Build' },
    ],
  });

  // One SWR entry covers all backends; share the same refresher so saves
  // refetch every config in lockstep without the page having to track
  // per-backend mutators.
  const { configs: backendConfigs, refresh: refreshBackendConfigs } = useBackendConfigs(
    SETTINGS_BACKEND_IDS
  );
  const opencodeBackendConfig = backendConfigs.opencode;
  const claudecodeBackendConfig = backendConfigs.claudecode;
  const grokBackendConfig = backendConfigs.grok;

  // Fetch Claude Code provider status (Anthropic provider configured for claudecode)
  const { data: claudecodeProvider } = useSWR<BackendProviderResponse>(
    'claudecode-provider',
    () => getProviderForBackend('claudecode'),
    { revalidateOnFocus: false }
  );

  useEffect(() => {
    if (!opencodeBackendConfig?.settings) return;
    const settings = opencodeBackendConfig.settings as Record<string, unknown>;
    setOpencodeForm({
      base_url: typeof settings.base_url === 'string' ? settings.base_url : '',
      default_agent: typeof settings.default_agent === 'string' ? settings.default_agent : '',
      permissive: Boolean(settings.permissive),
      enabled: opencodeBackendConfig.enabled,
    });
  }, [opencodeBackendConfig]);

  useEffect(() => {
    if (!claudecodeBackendConfig?.settings) return;
    const settings = claudecodeBackendConfig.settings as Record<string, unknown>;
    setClaudeForm((prev) => ({
      ...prev,
      cli_path: typeof settings.cli_path === 'string' ? settings.cli_path : '',
      api_key_configured: Boolean(settings.api_key_configured),
      enabled: claudecodeBackendConfig.enabled,
    }));
  }, [claudecodeBackendConfig]);

  useEffect(() => {
    if (!grokBackendConfig?.settings) return;
    const settings = grokBackendConfig.settings as Record<string, unknown>;
    setGrokForm({
      cli_path: typeof settings.cli_path === 'string' ? settings.cli_path : '',
      enabled: grokBackendConfig.enabled,
    });
  }, [grokBackendConfig]);

  useEffect(() => {
    const limit = serverSettings?.max_parallel_missions;
    if (typeof limit === 'number' && limit >= 1) {
      setMaxParallelMissionsValue(String(limit));
    }
    const taskLimit = serverSettings?.max_concurrent_tasks;
    if (typeof taskLimit === 'number' && taskLimit >= 1) {
      setMaxConcurrentTasksValue(String(taskLimit));
    }
  }, [serverSettings]);

  const handleSaveMissionLimit = async () => {
    const parsed = Number.parseInt(maxParallelMissionsValue, 10);
    if (!Number.isFinite(parsed) || parsed < 1) {
      toast.error('Max parallel missions must be at least 1');
      return;
    }

    setSavingMissionLimit(true);
    try {
      await updateSettings({ max_parallel_missions: parsed });
      await mutateSettings();
      toast.success('Mission concurrency limit updated');
    } catch (err) {
      toast.error(
        `Failed to update mission concurrency limit: ${
          err instanceof Error ? err.message : 'Unknown error'
        }`
      );
    } finally {
      setSavingMissionLimit(false);
    }
  };

  const handleSaveTaskLimit = async () => {
    const parsed = Number.parseInt(maxConcurrentTasksValue, 10);
    if (!Number.isFinite(parsed) || parsed < 1) {
      toast.error('Max concurrent tasks must be at least 1');
      return;
    }

    setSavingTaskLimit(true);
    try {
      await updateSettings({ max_concurrent_tasks: parsed });
      await mutateSettings();
      toast.success('Task concurrency limit updated');
    } catch (err) {
      toast.error(
        `Failed to update task concurrency limit: ${
          err instanceof Error ? err.message : 'Unknown error'
        }`
      );
    } finally {
      setSavingTaskLimit(false);
    }
  };

  const handleSaveOpenCodeBackend = async () => {
    setSavingBackend(true);
    try {
      const result = await updateBackendConfig(
        'opencode',
        {
          base_url: opencodeForm.base_url,
          default_agent: opencodeForm.default_agent || null,
          permissive: opencodeForm.permissive,
        },
        { enabled: opencodeForm.enabled }
      );
      toast.success(result.message || 'OpenCode settings updated');
      refreshBackendConfigs();
    } catch (err) {
      toast.error(
        `Failed to update OpenCode settings: ${
          err instanceof Error ? err.message : 'Unknown error'
        }`
      );
    } finally {
      setSavingBackend(false);
    }
  };

  const handleSaveClaudeBackend = async () => {
    setSavingBackend(true);
    try {
      const settings: Record<string, unknown> = {
        cli_path: claudeForm.cli_path || null,
      };

      const result = await updateBackendConfig('claudecode', settings, {
        enabled: claudeForm.enabled,
      });
      toast.success(result.message || 'Claude Code settings updated');
      refreshBackendConfigs();
    } catch (err) {
      toast.error(
        `Failed to update Claude Code settings: ${
          err instanceof Error ? err.message : 'Unknown error'
        }`
      );
    } finally {
      setSavingBackend(false);
    }
  };

  const handleSaveGrokBackend = async () => {
    setSavingBackend(true);
    try {
      const result = await updateBackendConfig(
        'grok',
        { cli_path: grokForm.cli_path || null },
        { enabled: grokForm.enabled }
      );
      toast.success(result.message || 'Grok Build settings updated');
      refreshBackendConfigs();
    } catch (err) {
      toast.error(
        `Failed to update Grok Build settings: ${
          err instanceof Error ? err.message : 'Unknown error'
        }`
      );
    } finally {
      setSavingBackend(false);
    }
  };

  return (
    <div className="flex-1 flex flex-col items-center p-6 overflow-auto">
      <div className="w-full max-w-xl space-y-6">
        {/* Header */}
        <div className="mb-8">
          <h1 className="text-xl font-semibold text-white">Backends</h1>
          <p className="mt-1 text-sm text-white/50">
            Configure AI coding agent harnesses
          </p>
        </div>

        {/* Server Connection */}
        <ServerConnectionCard
          apiUrl={apiUrl}
          setApiUrl={setApiUrl}
          urlError={urlError}
          validateUrl={validateUrl}
          health={health ?? null}
          healthLoading={healthLoading}
          testingConnection={testingConnection}
          testApiConnection={testApiConnection}
        />

        {/* Save URL button */}
        {hasUnsavedUrlChanges && (
          <div className="flex items-center justify-end gap-3 -mt-4 mb-6">
            <span className="text-xs text-amber-400">Unsaved changes</span>
            <button
              onClick={handleSaveUrl}
              disabled={!!urlError}
              className="flex items-center gap-2 rounded-lg bg-indigo-500 px-3 py-1.5 text-xs font-medium text-white hover:bg-indigo-600 transition-colors disabled:opacity-50 disabled:cursor-not-allowed"
            >
              <Save className="h-3.5 w-3.5" />
              Save URL
            </button>
          </div>
        )}

        {/* Backends */}
        <div className="rounded-xl bg-white/[0.02] border border-white/[0.04] p-5">
          <div className="flex items-center gap-3 mb-4">
            <div className="flex h-10 w-10 items-center justify-center rounded-xl bg-emerald-500/10">
              <Server className="h-5 w-5 text-emerald-400" />
            </div>
            <div>
              <h2 className="text-sm font-medium text-white">Backend Settings</h2>
              <p className="text-xs text-white/40">
                Configure execution backends and authentication
              </p>
            </div>
          </div>

          <div className="mb-4 rounded-lg border border-white/[0.06] bg-white/[0.02] p-3">
            <label className="block text-xs text-white/60 mb-1.5">
              Max Parallel Missions
            </label>
            <div className="flex items-center gap-2">
              <input
                type="number"
                min={1}
                step={1}
                value={maxParallelMissionsValue}
                onChange={(e) => setMaxParallelMissionsValue(e.target.value)}
                className="w-32 rounded-lg border border-white/[0.06] bg-white/[0.02] px-3 py-2 text-sm text-white focus:outline-none focus:border-indigo-500/50"
              />
              <button
                onClick={handleSaveMissionLimit}
                disabled={savingMissionLimit}
                className="flex items-center gap-2 rounded-lg bg-indigo-500 px-3 py-1.5 text-xs text-white hover:bg-indigo-600 transition-colors disabled:opacity-50"
              >
                {savingMissionLimit ? (
                  <Loader className="h-3.5 w-3.5 animate-spin" />
                ) : (
                  <Save className="h-3.5 w-3.5" />
                )}
                Save Limit
              </button>
            </div>
            <p className="mt-1.5 text-xs text-white/30">
              Global limit across all backends. Controls how many missions can run at the same time.
            </p>
          </div>

          <div className="mb-4 rounded-lg border border-white/[0.06] bg-white/[0.02] p-3">
            <label className="block text-xs text-white/60 mb-1.5">
              Max Concurrent Tasks
            </label>
            <div className="flex items-center gap-2">
              <input
                type="number"
                min={1}
                step={1}
                value={maxConcurrentTasksValue}
                onChange={(e) => setMaxConcurrentTasksValue(e.target.value)}
                className="w-32 rounded-lg border border-white/[0.06] bg-white/[0.02] px-3 py-2 text-sm text-white focus:outline-none focus:border-indigo-500/50"
              />
              <button
                onClick={handleSaveTaskLimit}
                disabled={savingTaskLimit}
                className="flex items-center gap-2 rounded-lg bg-indigo-500 px-3 py-1.5 text-xs text-white hover:bg-indigo-600 transition-colors disabled:opacity-50"
              >
                {savingTaskLimit ? (
                  <Loader className="h-3.5 w-3.5 animate-spin" />
                ) : (
                  <Save className="h-3.5 w-3.5" />
                )}
                Save Limit
              </button>
            </div>
            <p className="mt-1.5 text-xs text-white/30">
              Max command-mode tasks that can run simultaneously. Returns 429 when the limit is reached. Default: 5. Env: MAX_CONCURRENT_TASKS.
            </p>
          </div>

          <div className="flex items-center gap-2 mb-4">
            {backends.map((backend) => (
              <button
                key={backend.id}
                onClick={() =>
                  setActiveBackendTab(
                    backend.id as 'opencode' | 'claudecode' | 'grok'
                  )
                }
                className={cn(
                  'px-3 py-1.5 rounded-lg text-xs font-medium border transition-colors',
                  activeBackendTab === backend.id
                    ? 'bg-white/[0.08] border-white/[0.12] text-white'
                    : 'bg-white/[0.02] border-white/[0.06] text-white/50 hover:text-white/70'
                )}
              >
                {backend.name}
              </button>
            ))}
          </div>

          {activeBackendTab === 'opencode' ? (
            <div className="space-y-3">
              <label className="flex items-center gap-2 text-xs text-white/60 cursor-pointer">
                <input
                  type="checkbox"
                  checked={opencodeForm.enabled}
                  onChange={(e) =>
                    setOpencodeForm((prev) => ({ ...prev, enabled: e.target.checked }))
                  }
                  className="rounded border-white/20 cursor-pointer"
                />
                Enabled
              </label>
              <div>
                <label className="block text-xs text-white/60 mb-1.5">Base URL</label>
                <input
                  type="text"
                  value={opencodeForm.base_url}
                  onChange={(e) =>
                    setOpencodeForm((prev) => ({ ...prev, base_url: e.target.value }))
                  }
                  placeholder="http://127.0.0.1:4096"
                  className="w-full rounded-lg border border-white/[0.06] bg-white/[0.02] px-3 py-2 text-sm text-white focus:outline-none focus:border-indigo-500/50"
                />
              </div>
              <div>
                <label className="block text-xs text-white/60 mb-1.5">Default Agent</label>
                <input
                  type="text"
                  value={opencodeForm.default_agent}
                  onChange={(e) =>
                    setOpencodeForm((prev) => ({ ...prev, default_agent: e.target.value }))
                  }
                  placeholder="Sisyphus"
                  className="w-full rounded-lg border border-white/[0.06] bg-white/[0.02] px-3 py-2 text-sm text-white focus:outline-none focus:border-indigo-500/50"
                />
              </div>
              <label className="flex items-center gap-2 text-xs text-white/60 cursor-pointer">
                <input
                  type="checkbox"
                  checked={opencodeForm.permissive}
                  onChange={(e) =>
                    setOpencodeForm((prev) => ({ ...prev, permissive: e.target.checked }))
                  }
                  className="rounded border-white/20 cursor-pointer"
                />
                Permissive mode (auto-allow tool permissions)
              </label>
              <div className="flex items-center gap-2 pt-1">
                <button
                  onClick={handleSaveOpenCodeBackend}
                  disabled={savingBackend}
                  className="flex items-center gap-2 rounded-lg bg-indigo-500 px-3 py-1.5 text-xs text-white hover:bg-indigo-600 transition-colors disabled:opacity-50"
                >
                  {savingBackend ? (
                    <Loader className="h-3.5 w-3.5 animate-spin" />
                  ) : (
                    <Save className="h-3.5 w-3.5" />
                  )}
                  Save OpenCode
                </button>
              </div>
            </div>
          ) : activeBackendTab === 'claudecode' ? (
            <div className="space-y-3">
              <label className="flex items-center gap-2 text-xs text-white/60 cursor-pointer">
                <input
                  type="checkbox"
                  checked={claudeForm.enabled}
                  onChange={(e) =>
                    setClaudeForm((prev) => ({ ...prev, enabled: e.target.checked }))
                  }
                  className="rounded border-white/20 cursor-pointer"
                />
                Enabled
              </label>
              {/* Anthropic Provider Status */}
              <div className="flex items-center justify-between py-2 px-3 rounded-lg border border-white/[0.06] bg-white/[0.02]">
                <div className="flex items-center gap-2">
                  <span className="text-base">🧠</span>
                  <span className="text-sm text-white/70">
                    {claudecodeProvider?.configured
                      ? claudecodeProvider.auth_method === 'oauth'
                        ? 'OAuth'
                        : claudecodeProvider.auth_method === 'api_key'
                        ? 'API Key'
                        : 'Anthropic'
                      : 'Anthropic'}
                  </span>
                </div>
                {claudecodeProvider?.configured && claudecodeProvider.has_credentials ? (
                  <span className="flex items-center gap-1.5 text-xs text-emerald-400">
                    <Check className="h-3.5 w-3.5" />
                    Connected
                  </span>
                ) : (
                  <a
                    href="/settings"
                    className="text-xs text-amber-400 hover:text-amber-300 transition-colors"
                  >
                    Configure in AI Providers →
                  </a>
                )}
              </div>
              <div>
                <label className="block text-xs text-white/60 mb-1.5">CLI Path</label>
                <input
                  type="text"
                  value={claudeForm.cli_path || ''}
                  onChange={(e) =>
                    setClaudeForm((prev) => ({ ...prev, cli_path: e.target.value }))
                  }
                  placeholder="claude (uses PATH) or /path/to/claude"
                  className="w-full rounded-lg border border-white/[0.06] bg-white/[0.02] px-3 py-2 text-sm text-white focus:outline-none focus:border-indigo-500/50"
                />
                <p className="mt-1.5 text-xs text-white/30">
                  Path to the Claude CLI executable. Leave blank to use default from PATH.
                </p>
              </div>
              <div className="flex items-center gap-2 pt-1">
                <button
                  onClick={handleSaveClaudeBackend}
                  disabled={savingBackend}
                  className="flex items-center gap-2 rounded-lg bg-indigo-500 px-3 py-1.5 text-xs text-white hover:bg-indigo-600 transition-colors disabled:opacity-50"
                >
                  {savingBackend ? (
                    <Loader className="h-3.5 w-3.5 animate-spin" />
                  ) : (
                    <Save className="h-3.5 w-3.5" />
                  )}
                  Save Claude Code
                </button>
              </div>
            </div>
          ) : activeBackendTab === 'grok' ? (
            <div className="space-y-3">
              <label className="flex items-center gap-2 text-xs text-white/60 cursor-pointer">
                <input
                  type="checkbox"
                  checked={grokForm.enabled}
                  onChange={(e) =>
                    setGrokForm((prev) => ({ ...prev, enabled: e.target.checked }))
                  }
                  className="rounded border-white/20 cursor-pointer"
                />
                Enabled
              </label>
              <div className="flex items-center justify-between py-2 px-3 rounded-lg border border-white/[0.06] bg-white/[0.02]">
                <div className="flex items-center gap-2">
                  <span className="text-base">𝕏</span>
                  <span className="text-sm text-white/70">xAI provider or X login</span>
                </div>
                <a
                  href="/settings/providers"
                  className="text-xs text-indigo-400 hover:text-indigo-300 transition-colors"
                >
                  Configure provider →
                </a>
              </div>
              <div>
                <label className="block text-xs text-white/60 mb-1.5">CLI Path</label>
                <input
                  type="text"
                  value={grokForm.cli_path || ''}
                  onChange={(e) =>
                    setGrokForm((prev) => ({ ...prev, cli_path: e.target.value }))
                  }
                  placeholder="grok (uses PATH) or /path/to/grok"
                  className="w-full rounded-lg border border-white/[0.06] bg-white/[0.02] px-3 py-2 text-sm text-white focus:outline-none focus:border-indigo-500/50"
                />
                <p className="mt-1.5 text-xs text-white/30">
                  Grok opens a browser for X authentication on first launch. In headless environments, configure an xAI provider for Grok Build.
                </p>
              </div>
              <div className="flex items-center gap-2 pt-1">
                <button
                  onClick={handleSaveGrokBackend}
                  disabled={savingBackend}
                  className="flex items-center gap-2 rounded-lg bg-indigo-500 px-3 py-1.5 text-xs text-white hover:bg-indigo-600 transition-colors disabled:opacity-50"
                >
                  {savingBackend ? (
                    <Loader className="h-3.5 w-3.5 animate-spin" />
                  ) : (
                    <Save className="h-3.5 w-3.5" />
                  )}
                  Save Grok Build
                </button>
              </div>
            </div>
          ) : null}
        </div>
      </div>
    </div>
  );
}
