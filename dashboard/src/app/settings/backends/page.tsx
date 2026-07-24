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
import { Save, Loader, Check, Gauge, Bot, KeyRound, ServerCog, WandSparkles } from 'lucide-react';
import { cn } from '@/lib/utils';
import { getRuntimeApiBase, writeSavedSettings } from '@/lib/settings';
import { ServerConnectionCard } from '@/components/server-connection-card';
import { ModelRoutingDebug } from '@/components/model-routing-debug';
import { useBackendConfigs } from '@/lib/use-backend-configs';

const SETTINGS_BACKEND_IDS = ['opencode', 'claudecode', 'grok', 'chatgpt_ui'] as const;
type SettingsBackendId = typeof SETTINGS_BACKEND_IDS[number];

const SETTINGS_BACKEND_NAMES: Record<SettingsBackendId, string> = {
  opencode: 'OpenCode',
  claudecode: 'Claude Code',
  grok: 'Grok Build',
  chatgpt_ui: 'ChatGPT UI (experimental)',
};

const CHATGPT_UI_PRODUCTION_DEFAULTS = {
  profile_dir: '/var/lib/sandboxed-sh/chatgpt-profile',
  driver_path: '/opt/sandboxed-sh/scripts/chatgpt_ui_driver.py',
  python_path: '/opt/sandboxed-sh/chatgpt-ui-venv/bin/python',
};

export default function BackendsPage() {
  const [activeBackendTab, setActiveBackendTab] = useState<SettingsBackendId>('opencode');
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
  const [chatgptUiForm, setChatgptUiForm] = useState({
    profile_dir: '',
    driver_path: '',
    python_path: 'python3',
    model: '',
    timeout_secs: 900,
    headless: true,
    enabled: false,
  });

  // SWR: fetch backends
  const { data: backends = [] } = useSWR('backends', listBackends, {
    revalidateOnFocus: false,
    fallbackData: [
      { id: 'opencode', name: 'OpenCode' },
      { id: 'claudecode', name: 'Claude Code' },
      { id: 'grok', name: 'Grok Build' },
      { id: 'chatgpt_ui', name: 'ChatGPT UI (experimental)' },
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
  const chatgptUiBackendConfig = backendConfigs.chatgpt_ui;
  const configurableBackends = SETTINGS_BACKEND_IDS.map((id) => ({
    id,
    name: backends.find((backend) => backend.id === id)?.name || SETTINGS_BACKEND_NAMES[id],
  }));
  const chatgptUiIsConfigured = Boolean(
    chatgptUiForm.profile_dir.trim()
      && chatgptUiForm.driver_path.trim()
      && chatgptUiForm.python_path.trim()
  );

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
    if (!chatgptUiBackendConfig?.settings) return;
    const settings = chatgptUiBackendConfig.settings as Record<string, unknown>;
    setChatgptUiForm({
      profile_dir: typeof settings.profile_dir === 'string' ? settings.profile_dir : '',
      driver_path: typeof settings.driver_path === 'string' ? settings.driver_path : '',
      python_path: typeof settings.python_path === 'string' ? settings.python_path : 'python3',
      model: typeof settings.model === 'string' ? settings.model : '',
      timeout_secs: typeof settings.timeout_secs === 'number' ? settings.timeout_secs : 900,
      headless: settings.headless !== false,
      enabled: chatgptUiBackendConfig.enabled,
    });
  }, [chatgptUiBackendConfig]);

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

  const handleSaveChatgptUiBackend = async () => {
    setSavingBackend(true);
    try {
      const result = await updateBackendConfig(
        'chatgpt_ui',
        {
          profile_dir: chatgptUiForm.profile_dir,
          driver_path: chatgptUiForm.driver_path || null,
          python_path: chatgptUiForm.python_path || 'python3',
          browser: 'chromium',
          headless: chatgptUiForm.headless,
          timeout_secs: chatgptUiForm.timeout_secs,
          model: chatgptUiForm.model || null,
        },
        { enabled: chatgptUiForm.enabled }
      );
      toast.success(result.message || 'ChatGPT UI settings updated');
      refreshBackendConfigs();
    } catch (err) {
      toast.error(`Failed to update ChatGPT UI settings: ${err instanceof Error ? err.message : 'Unknown error'}`);
    } finally {
      setSavingBackend(false);
    }
  };

  return (
    <div className="flex-1 flex flex-col items-center p-6 overflow-auto">
      <div className="w-full max-w-4xl space-y-6">
        {/* Header */}
        <header>
          <h1 className="text-xl font-semibold text-white">Backends</h1>
          <p className="mt-1 text-sm text-white/50">
            Configure harnesses, installs, and runtime limits
          </p>
        </header>

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
          <div className="flex items-center justify-end gap-3 -mt-3">
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

        {/* Concurrency Limits */}
        <section className="rounded-xl bg-white/[0.02] border border-white/[0.04] p-5">
          <div className="flex items-center gap-3 mb-4">
            <div className="flex h-10 w-10 items-center justify-center rounded-xl bg-amber-500/10 flex-shrink-0">
              <Gauge className="h-5 w-5 text-amber-400" />
            </div>
            <div className="min-w-0">
              <h2 className="text-sm font-medium text-white">Concurrency Limits</h2>
              <p className="text-xs text-white/40 truncate">
                Global execution caps applied across all missions and tasks
              </p>
            </div>
          </div>

          <div className="grid gap-3 sm:grid-cols-2">
            <div className="rounded-lg border border-white/[0.06] bg-white/[0.02] p-3">
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
                  className="min-w-0 flex-1 rounded-lg border border-white/[0.06] bg-white/[0.02] px-3 py-2 text-sm text-white focus:outline-none focus:border-indigo-500/50"
                />
                <button
                  onClick={handleSaveMissionLimit}
                  disabled={savingMissionLimit}
                  className="flex h-9 w-9 flex-shrink-0 items-center justify-center rounded-lg bg-indigo-500 text-white hover:bg-indigo-600 transition-colors disabled:opacity-50"
                  title="Save mission limit"
                >
                  {savingMissionLimit ? (
                    <Loader className="h-3.5 w-3.5 animate-spin" />
                  ) : (
                    <Save className="h-3.5 w-3.5" />
                  )}
                </button>
              </div>
              <p className="mt-1.5 text-xs text-white/30">
                Global mission concurrency.
              </p>
            </div>

            <div className="rounded-lg border border-white/[0.06] bg-white/[0.02] p-3">
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
                  className="min-w-0 flex-1 rounded-lg border border-white/[0.06] bg-white/[0.02] px-3 py-2 text-sm text-white focus:outline-none focus:border-indigo-500/50"
                />
                <button
                  onClick={handleSaveTaskLimit}
                  disabled={savingTaskLimit}
                  className="flex h-9 w-9 flex-shrink-0 items-center justify-center rounded-lg bg-indigo-500 text-white hover:bg-indigo-600 transition-colors disabled:opacity-50"
                  title="Save task limit"
                >
                  {savingTaskLimit ? (
                    <Loader className="h-3.5 w-3.5 animate-spin" />
                  ) : (
                    <Save className="h-3.5 w-3.5" />
                  )}
                </button>
              </div>
              <p className="mt-1.5 text-xs text-white/30">
                Command-mode task concurrency.
              </p>
            </div>
          </div>
        </section>

        {/* Backends */}
        <section className="rounded-xl bg-white/[0.02] border border-white/[0.04] p-5">
          <div className="flex items-center gap-3 mb-4">
            <div className="flex h-10 w-10 items-center justify-center rounded-xl bg-emerald-500/10 flex-shrink-0">
              <Bot className="h-5 w-5 text-emerald-400" />
            </div>
            <div className="min-w-0">
              <h2 className="text-sm font-medium text-white">Harness Settings</h2>
              <p className="text-xs text-white/40 truncate">
                Per-harness defaults and authentication
              </p>
            </div>
          </div>

          <div className="flex flex-wrap items-center gap-2 mb-4">
            {configurableBackends.map((backend) => (
              <button
                key={backend.id}
                onClick={() =>
                  setActiveBackendTab(
                    backend.id as SettingsBackendId
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
                  placeholder="build"
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
          ) : activeBackendTab === 'chatgpt_ui' ? (
            <div className="space-y-4">
              <div className="rounded-xl border border-amber-500/20 bg-amber-500/[0.06] p-4">
                <div className="flex items-start gap-3">
                  <KeyRound className="mt-0.5 h-4 w-4 shrink-0 text-amber-300" />
                  <div>
                    <p className="text-sm font-medium text-amber-100">Dedicated ChatGPT web login</p>
                    <p className="mt-1 text-xs leading-5 text-amber-100/60">
                      This harness does not reuse the OpenAI or Codex OAuth configured in provider settings.
                      Sign in once inside the dedicated Playwright browser profile; never place that profile
                      in a repository or mission workspace.
                    </p>
                  </div>
                </div>
              </div>

              <div className="flex flex-col gap-3 rounded-xl border border-white/[0.06] bg-white/[0.02] p-4 sm:flex-row sm:items-center sm:justify-between">
                <div className="flex items-center gap-3">
                  <div className={cn(
                    'flex h-9 w-9 items-center justify-center rounded-lg',
                    chatgptUiIsConfigured ? 'bg-emerald-500/10' : 'bg-white/[0.04]'
                  )}>
                    <ServerCog className={cn(
                      'h-4 w-4',
                      chatgptUiIsConfigured ? 'text-emerald-400' : 'text-white/35'
                    )} />
                  </div>
                  <div>
                    <p className="text-sm text-white">
                      {chatgptUiIsConfigured ? 'Runtime paths configured' : 'Runtime paths required'}
                    </p>
                    <p className="mt-0.5 text-xs text-white/35">
                      Paths are resolved on the connected Sandboxed.sh server.
                    </p>
                  </div>
                </div>
                <label className="flex items-center gap-2 text-xs text-white/60 cursor-pointer">
                  <input
                    type="checkbox"
                    checked={chatgptUiForm.enabled}
                    onChange={(e) => setChatgptUiForm((prev) => ({ ...prev, enabled: e.target.checked }))}
                    className="rounded border-white/20 cursor-pointer"
                  />
                  Enable harness
                </label>
              </div>

              <div className="flex flex-wrap items-center justify-between gap-2">
                <p className="text-xs font-medium uppercase tracking-[0.16em] text-white/35">
                  Server paths
                </p>
                <button
                  type="button"
                  onClick={() => setChatgptUiForm((prev) => ({
                    ...prev,
                    ...CHATGPT_UI_PRODUCTION_DEFAULTS,
                  }))}
                  className="flex items-center gap-1.5 rounded-lg border border-indigo-400/20 bg-indigo-400/[0.06] px-2.5 py-1.5 text-xs text-indigo-300 transition-colors hover:bg-indigo-400/[0.1]"
                >
                  <WandSparkles className="h-3.5 w-3.5" />
                  Use production defaults
                </button>
              </div>
              <div>
                <label htmlFor="chatgpt-ui-profile-dir" className="block text-xs text-white/60 mb-1.5">Browser profile directory</label>
                <input id="chatgpt-ui-profile-dir" type="text" value={chatgptUiForm.profile_dir} onChange={(e) => setChatgptUiForm((prev) => ({ ...prev, profile_dir: e.target.value }))} placeholder="/var/lib/sandboxed-sh/chatgpt-profile" className="w-full rounded-lg border border-white/[0.06] bg-white/[0.02] px-3 py-2 text-sm text-white focus:outline-none focus:border-indigo-500/50" />
                <p className="mt-1.5 text-xs text-white/30">
                  Persistent browser state containing the separate chatgpt.com login.
                </p>
              </div>
              <div>
                <label htmlFor="chatgpt-ui-driver-path" className="block text-xs text-white/60 mb-1.5">Driver path</label>
                <input id="chatgpt-ui-driver-path" type="text" value={chatgptUiForm.driver_path} onChange={(e) => setChatgptUiForm((prev) => ({ ...prev, driver_path: e.target.value }))} placeholder="/opt/sandboxed-sh/scripts/chatgpt_ui_driver.py" className="w-full rounded-lg border border-white/[0.06] bg-white/[0.02] px-3 py-2 text-sm text-white focus:outline-none focus:border-indigo-500/50" />
              </div>
              <div>
                <label htmlFor="chatgpt-ui-python-path" className="block text-xs text-white/60 mb-1.5">Python executable</label>
                <input id="chatgpt-ui-python-path" type="text" value={chatgptUiForm.python_path} onChange={(e) => setChatgptUiForm((prev) => ({ ...prev, python_path: e.target.value }))} placeholder="/opt/sandboxed-sh/chatgpt-ui-venv/bin/python" className="w-full rounded-lg border border-white/[0.06] bg-white/[0.02] px-3 py-2 text-sm text-white focus:outline-none focus:border-indigo-500/50" />
              </div>
              <div className="grid grid-cols-1 gap-3 sm:grid-cols-2">
                <div>
                  <label htmlFor="chatgpt-ui-model" className="block text-xs text-white/60 mb-1.5">Exact visible model label</label>
                  <input id="chatgpt-ui-model" type="text" value={chatgptUiForm.model} onChange={(e) => setChatgptUiForm((prev) => ({ ...prev, model: e.target.value }))} placeholder="GPT-5.6 Pro" className="w-full rounded-lg border border-white/[0.06] bg-white/[0.02] px-3 py-2 text-sm text-white focus:outline-none focus:border-indigo-500/50" />
                </div>
                <div>
                  <label htmlFor="chatgpt-ui-timeout" className="block text-xs text-white/60 mb-1.5">Timeout (seconds)</label>
                  <input id="chatgpt-ui-timeout" type="number" min={30} max={7200} value={chatgptUiForm.timeout_secs} onChange={(e) => setChatgptUiForm((prev) => ({ ...prev, timeout_secs: Number(e.target.value) }))} className="w-full rounded-lg border border-white/[0.06] bg-white/[0.02] px-3 py-2 text-sm text-white focus:outline-none focus:border-indigo-500/50" />
                </div>
              </div>
              <label className="flex items-center gap-2 text-xs text-white/60 cursor-pointer">
                <input type="checkbox" checked={chatgptUiForm.headless} onChange={(e) => setChatgptUiForm((prev) => ({ ...prev, headless: e.target.checked }))} />
                Headless after interactive login provisioning
              </label>
              <button
                onClick={handleSaveChatgptUiBackend}
                disabled={savingBackend || !chatgptUiIsConfigured || chatgptUiForm.timeout_secs < 30 || chatgptUiForm.timeout_secs > 7200}
                className="flex items-center gap-2 rounded-lg bg-indigo-500 px-3 py-1.5 text-xs text-white hover:bg-indigo-600 transition-colors disabled:cursor-not-allowed disabled:opacity-40"
              >
                {savingBackend ? <Loader className="h-3.5 w-3.5 animate-spin" /> : <Save className="h-3.5 w-3.5" />}
                Save ChatGPT UI
              </button>
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
        </section>

        <ModelRoutingDebug />
      </div>
    </div>
  );
}
