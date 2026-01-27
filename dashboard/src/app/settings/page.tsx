'use client';

import { useState, useEffect, useCallback, useRef } from 'react';
import useSWR from 'swr';
import { toast } from '@/components/toast';
import {
  getHealth,
  HealthResponse,
  listAIProviders,
  listAIProviderTypes,
  updateAIProvider,
  deleteAIProvider,
  authenticateAIProvider,
  setDefaultAIProvider,
  AIProvider,
  AIProviderTypeInfo,
  getSettings,
  updateLibraryRemote,
  listBackends,
  getBackendConfig,
  updateBackendConfig,
  getProviderForBackend,
  BackendProviderResponse,
  downloadBackup,
  restoreBackup,
} from '@/lib/api';
import {
  Server,
  Save,
  RefreshCw,
  AlertTriangle,
  GitBranch,
  Cpu,
  Plus,
  Trash2,
  Star,
  ExternalLink,
  Loader,
  Key,
  Check,
  X,
  Download,
  Upload,
  Archive,
} from 'lucide-react';
import { readSavedSettings, writeSavedSettings } from '@/lib/settings';
import { cn } from '@/lib/utils';
import { AddProviderModal } from '@/components/ui/add-provider-modal';
import { ServerConnectionCard } from '@/components/server-connection-card';

// Provider icons/colors mapping
const providerConfig: Record<string, { color: string; icon: string }> = {
  anthropic: { color: 'bg-orange-500/10 text-orange-400', icon: '🧠' },
  openai: { color: 'bg-emerald-500/10 text-emerald-400', icon: '🤖' },
  google: { color: 'bg-blue-500/10 text-blue-400', icon: '🔮' },
  'amazon-bedrock': { color: 'bg-amber-500/10 text-amber-400', icon: '☁️' },
  azure: { color: 'bg-sky-500/10 text-sky-400', icon: '⚡' },
  'open-router': { color: 'bg-purple-500/10 text-purple-400', icon: '🔀' },
  mistral: { color: 'bg-indigo-500/10 text-indigo-400', icon: '🌪️' },
  groq: { color: 'bg-pink-500/10 text-pink-400', icon: '⚡' },
  xai: { color: 'bg-slate-500/10 text-slate-400', icon: '𝕏' },
  zai: { color: 'bg-cyan-500/10 text-cyan-400', icon: 'Z' },
  'github-copilot': { color: 'bg-gray-500/10 text-gray-400', icon: '🐙' },
  custom: { color: 'bg-white/10 text-white/60', icon: '🔧' },
};

function getProviderConfig(type: string) {
  return providerConfig[type] || providerConfig.custom;
}

// Default provider types fallback
const defaultProviderTypes: AIProviderTypeInfo[] = [
  { id: 'anthropic', name: 'Anthropic', uses_oauth: true, env_var: 'ANTHROPIC_API_KEY' },
  { id: 'openai', name: 'OpenAI', uses_oauth: true, env_var: 'OPENAI_API_KEY' },
  { id: 'google', name: 'Google AI', uses_oauth: true, env_var: 'GOOGLE_API_KEY' },
  { id: 'open-router', name: 'OpenRouter', uses_oauth: false, env_var: 'OPENROUTER_API_KEY' },
  { id: 'groq', name: 'Groq', uses_oauth: false, env_var: 'GROQ_API_KEY' },
  { id: 'mistral', name: 'Mistral AI', uses_oauth: false, env_var: 'MISTRAL_API_KEY' },
  { id: 'xai', name: 'xAI', uses_oauth: false, env_var: 'XAI_API_KEY' },
  { id: 'zai', name: 'Z.AI', uses_oauth: false, env_var: 'ZHIPU_API_KEY' },
  { id: 'github-copilot', name: 'GitHub Copilot', uses_oauth: true, env_var: null },
];

export default function SettingsPage() {
  const [testingConnection, setTestingConnection] = useState(false);

  // Form state
  const [apiUrl, setApiUrl] = useState(
    () => readSavedSettings().apiUrl ?? 'http://127.0.0.1:3000'
  );

  // Track original values for unsaved changes
  const [originalValues, setOriginalValues] = useState({
    apiUrl: readSavedSettings().apiUrl ?? 'http://127.0.0.1:3000',
  });

  // Validation state
  const [urlError, setUrlError] = useState<string | null>(null);

  // Modal/edit state
  const [showAddModal, setShowAddModal] = useState(false);
  const [authenticatingProviderId, setAuthenticatingProviderId] = useState<string | null>(null);
  const [editingProvider, setEditingProvider] = useState<string | null>(null);
  const [editForm, setEditForm] = useState<{
    name?: string;
    google_project_id?: string;
    api_key?: string;
    base_url?: string;
    enabled?: boolean;
  }>({});

  // Library remote edit state
  const [editingLibraryRemote, setEditingLibraryRemote] = useState(false);
  const [libraryRemoteValue, setLibraryRemoteValue] = useState('');
  const [savingLibraryRemote, setSavingLibraryRemote] = useState(false);

  // Backend settings state
  const [activeBackendTab, setActiveBackendTab] = useState<'opencode' | 'claudecode' | 'amp'>('opencode');
  const [savingBackend, setSavingBackend] = useState(false);
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
  const [ampForm, setAmpForm] = useState({
    cli_path: '',
    default_mode: 'smart',
    permissive: true,
    enabled: true,
    api_key: '',
  });

  // Backup/restore state
  const [downloadingBackup, setDownloadingBackup] = useState(false);
  const [restoringBackup, setRestoringBackup] = useState(false);
  const fileInputRef = useCallback((node: HTMLInputElement | null) => {
    if (node) {
      node.value = '';
    }
  }, []);

  // SWR: fetch health status
  const { data: health, isLoading: healthLoading, mutate: mutateHealth } = useSWR(
    'health',
    getHealth,
    { revalidateOnFocus: false }
  );

  // SWR: fetch AI providers
  const { data: providers = [], isLoading: providersLoading, mutate: mutateProviders } = useSWR(
    'ai-providers',
    listAIProviders,
    { revalidateOnFocus: false }
  );

  // SWR: fetch provider types (with fallback)
  const { data: providerTypes = defaultProviderTypes } = useSWR(
    'ai-provider-types',
    listAIProviderTypes,
    { revalidateOnFocus: false, fallbackData: defaultProviderTypes }
  );

  // SWR: fetch server settings
  const { data: serverSettings, mutate: mutateSettings } = useSWR(
    'settings',
    getSettings,
    { revalidateOnFocus: false }
  );

  // SWR: fetch backends
  const { data: backends = [] } = useSWR('backends', listBackends, {
    revalidateOnFocus: false,
    fallbackData: [
      { id: 'opencode', name: 'OpenCode' },
      { id: 'claudecode', name: 'Claude Code' },
    ],
  });

  const { data: opencodeBackendConfig, mutate: mutateOpenCodeBackend } = useSWR(
    'backend-opencode-config',
    () => getBackendConfig('opencode'),
    { revalidateOnFocus: false }
  );
  const { data: claudecodeBackendConfig, mutate: mutateClaudeBackend } = useSWR(
    'backend-claudecode-config',
    () => getBackendConfig('claudecode'),
    { revalidateOnFocus: false }
  );
  const { data: ampBackendConfig, mutate: mutateAmpBackend } = useSWR(
    'backend-amp-config',
    () => getBackendConfig('amp'),
    { revalidateOnFocus: false }
  );

  // Fetch Claude Code provider status (Anthropic provider configured for claudecode)
  const { data: claudecodeProvider, mutate: mutateClaudeProvider } = useSWR<BackendProviderResponse>(
    'claudecode-provider',
    () => getProviderForBackend('claudecode'),
    { revalidateOnFocus: false }
  );

  // Check if there are unsaved changes
  const hasUnsavedChanges = apiUrl !== originalValues.apiUrl;

  // Validate URL
  const validateUrl = useCallback((url: string) => {
    if (!url.trim()) {
      setUrlError('API URL is required');
      return false;
    }
    try {
      new URL(url);
      setUrlError(null);
      return true;
    } catch {
      setUrlError('Invalid URL format');
      return false;
    }
  }, []);


  // Unsaved changes warning
  useEffect(() => {
    const handleBeforeUnload = (e: BeforeUnloadEvent) => {
      if (hasUnsavedChanges) {
        e.preventDefault();
        e.returnValue = '';
      }
    };

    window.addEventListener('beforeunload', handleBeforeUnload);
    return () => window.removeEventListener('beforeunload', handleBeforeUnload);
  }, [hasUnsavedChanges]);

  // Keyboard shortcut to save (Ctrl/Cmd + S)
  useEffect(() => {
    const handleKeyDown = (e: KeyboardEvent) => {
      if ((e.metaKey || e.ctrlKey) && e.key === 's') {
        e.preventDefault();
        handleSave();
      }
    };

    window.addEventListener('keydown', handleKeyDown);
    return () => window.removeEventListener('keydown', handleKeyDown);
  }, [apiUrl]);

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
    if (!ampBackendConfig?.settings) return;
    const settings = ampBackendConfig.settings as Record<string, unknown>;
    setAmpForm({
      cli_path: typeof settings.cli_path === 'string' ? settings.cli_path : '',
      default_mode: typeof settings.default_mode === 'string' ? settings.default_mode : 'smart',
      permissive: settings.permissive !== false,
      enabled: ampBackendConfig.enabled,
      api_key: typeof settings.api_key === 'string' ? settings.api_key : '',
    });
  }, [ampBackendConfig]);

  const handleSave = () => {
    if (!validateUrl(apiUrl)) {
      toast.error('Please fix validation errors before saving');
      return;
    }

    writeSavedSettings({ apiUrl });
    setOriginalValues({ apiUrl });
    toast.success('Settings saved!');
  };

  const testApiConnection = async () => {
    if (!validateUrl(apiUrl)) {
      toast.error('Please enter a valid API URL');
      return;
    }

    setTestingConnection(true);
    try {
      const response = await fetch(`${apiUrl}/api/health`);
      if (!response.ok) {
        throw new Error(`HTTP ${response.status}`);
      }
      const data = await response.json();
      mutateHealth(data, false); // Update cache without revalidation
      toast.success(`Connected to OpenAgent v${data.version}`);
    } catch (err) {
      mutateHealth(undefined, false); // Clear cache on error
      toast.error(
        `Connection failed: ${err instanceof Error ? err.message : 'Unknown error'}`
      );
    } finally {
      setTestingConnection(false);
    }
  };

  const handleAuthenticate = async (provider: AIProvider) => {
    setAuthenticatingProviderId(provider.id);
    try {
      const result = await authenticateAIProvider(provider.id);
      if (result.success) {
        toast.success(result.message);
        mutateProviders();
      } else {
        if (result.auth_url) {
          window.open(result.auth_url, '_blank');
          toast.info(result.message);
        } else {
          toast.error(result.message);
        }
      }
    } catch (err) {
      toast.error(
        `Authentication failed: ${err instanceof Error ? err.message : 'Unknown error'}`
      );
    } finally {
      setAuthenticatingProviderId(null);
    }
  };

  const handleSetDefault = async (id: string) => {
    try {
      await setDefaultAIProvider(id);
      toast.success('Default provider updated');
      mutateProviders();
    } catch (err) {
      toast.error(
        `Failed to set default: ${err instanceof Error ? err.message : 'Unknown error'}`
      );
    }
  };

  const handleDeleteProvider = async (id: string) => {
    try {
      await deleteAIProvider(id);
      toast.success('Provider removed');
      mutateProviders();
    } catch (err) {
      toast.error(
        `Failed to delete: ${err instanceof Error ? err.message : 'Unknown error'}`
      );
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
      mutateOpenCodeBackend();
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
      mutateClaudeBackend();
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

  const handleSaveAmpBackend = async () => {
    setSavingBackend(true);
    try {
      const settings: Record<string, unknown> = {
        cli_path: ampForm.cli_path || null,
        default_mode: ampForm.default_mode || 'smart',
        permissive: ampForm.permissive,
        api_key: ampForm.api_key || null,
      };

      const result = await updateBackendConfig('amp', settings, {
        enabled: ampForm.enabled,
      });
      toast.success(result.message || 'Amp settings updated');
      mutateAmpBackend();
    } catch (err) {
      toast.error(
        `Failed to update Amp settings: ${
          err instanceof Error ? err.message : 'Unknown error'
        }`
      );
    } finally {
      setSavingBackend(false);
    }
  };

  const handleStartEdit = (provider: AIProvider) => {
    setEditingProvider(provider.id);
    setEditForm({
      name: provider.name,
      google_project_id: provider.google_project_id ?? '',
      api_key: '',
      base_url: provider.base_url || '',
      enabled: provider.enabled,
    });
  };

  const handleSaveEdit = async () => {
    if (!editingProvider) return;

    try {
      await updateAIProvider(editingProvider, {
        name: editForm.name,
        google_project_id:
          editForm.google_project_id === ''
            ? null
            : editForm.google_project_id || undefined,
        api_key: editForm.api_key || undefined,
        base_url: editForm.base_url || undefined,
        enabled: editForm.enabled,
      });
      toast.success('Provider updated');
      setEditingProvider(null);
      mutateProviders();
    } catch (err) {
      toast.error(
        `Failed to update: ${err instanceof Error ? err.message : 'Unknown error'}`
      );
    }
  };

  const handleCancelEdit = () => {
    setEditingProvider(null);
    setEditForm({});
  };

  // Library remote handlers
  const handleStartEditLibraryRemote = () => {
    setLibraryRemoteValue(serverSettings?.library_remote || '');
    setEditingLibraryRemote(true);
  };

  const handleCancelEditLibraryRemote = () => {
    setEditingLibraryRemote(false);
    setLibraryRemoteValue('');
  };

  const handleSaveLibraryRemote = async () => {
    setSavingLibraryRemote(true);
    try {
      const trimmed = libraryRemoteValue.trim();
      const result = await updateLibraryRemote(trimmed || null);

      // Revalidate both settings and health (which also exposes library_remote)
      mutateSettings();
      mutateHealth();

      setEditingLibraryRemote(false);

      if (result.library_reinitialized) {
        if (result.library_error) {
          toast.error(`Library saved but failed to initialize: ${result.library_error}`);
        } else if (result.library_remote) {
          toast.success('Library remote updated and reinitialized');
        } else {
          toast.success('Library remote cleared');
        }
      } else {
        toast.success('Library remote saved (no change)');
      }
    } catch (err) {
      toast.error(
        `Failed to save: ${err instanceof Error ? err.message : 'Unknown error'}`
      );
    } finally {
      setSavingLibraryRemote(false);
    }
  };

  // Backup/restore handlers
  const handleDownloadBackup = async () => {
    setDownloadingBackup(true);
    try {
      await downloadBackup();
      toast.success('Backup downloaded successfully');
    } catch (err) {
      toast.error(
        `Failed to download backup: ${err instanceof Error ? err.message : 'Unknown error'}`
      );
    } finally {
      setDownloadingBackup(false);
    }
  };

  const handleRestoreBackup = async (file: File) => {
    setRestoringBackup(true);
    try {
      const result = await restoreBackup(file);
      if (result.success) {
        toast.success(result.message);
        // Refresh all data after restore
        mutateHealth();
        mutateSettings();
        mutateProviders();
        mutateOpenCodeBackend();
        mutateClaudeBackend();
        mutateAmpBackend();
      } else {
        toast.error(result.message);
        if (result.errors.length > 0) {
          result.errors.forEach((error) => toast.error(error));
        }
      }
    } catch (err) {
      toast.error(
        `Failed to restore backup: ${err instanceof Error ? err.message : 'Unknown error'}`
      );
    } finally {
      setRestoringBackup(false);
    }
  };

  return (
    <div className="min-h-screen flex flex-col items-center p-6">
      {/* Add Provider Modal */}
      <AddProviderModal
        open={showAddModal}
        onClose={() => setShowAddModal(false)}
        onSuccess={() => mutateProviders()}
        providerTypes={providerTypes}
      />

      {/* Centered content container */}
      <div className="w-full max-w-xl">
        {/* Header */}
        <div className="mb-8 flex items-center justify-between">
          <div>
            <h1 className="text-xl font-semibold text-white">Settings</h1>
            <p className="mt-1 text-sm text-white/50">
              Configure your server connection and AI providers
            </p>
          </div>
          <div className="flex items-center gap-3">
            {hasUnsavedChanges && (
              <div className="flex items-center gap-2 text-amber-400 text-xs">
                <AlertTriangle className="h-3.5 w-3.5" />
                <span>Unsaved</span>
              </div>
            )}
            <button
              onClick={handleSave}
              disabled={!!urlError}
              className={cn(
                'flex items-center gap-2 rounded-lg px-3 py-1.5 text-sm font-medium text-white transition-colors cursor-pointer',
                urlError
                  ? 'bg-white/10 cursor-not-allowed opacity-50'
                  : 'bg-indigo-500 hover:bg-indigo-600'
              )}
            >
              <Save className="h-4 w-4" />
              Save
              <span className="text-xs text-white/40">⌘S</span>
            </button>
          </div>
        </div>

        <div className="space-y-5">
          {/* Server Connection & System Components */}
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

          {/* AI Providers */}
          <div id="ai-providers" className="rounded-xl bg-white/[0.02] border border-white/[0.04] p-5">
            <div className="flex items-center justify-between mb-4">
              <div className="flex items-center gap-3">
                <div className="flex h-10 w-10 items-center justify-center rounded-xl bg-violet-500/10">
                  <Cpu className="h-5 w-5 text-violet-400" />
                </div>
                <div>
                  <h2 className="text-sm font-medium text-white">AI Providers</h2>
                  <p className="text-xs text-white/40">
                    Configure inference providers for OpenCode and Claude Code
                  </p>
                </div>
              </div>
              <button
                onClick={() => setShowAddModal(true)}
                className="flex items-center gap-1.5 rounded-lg border border-white/[0.06] bg-white/[0.02] px-3 py-1.5 text-xs text-white/70 hover:bg-white/[0.04] transition-colors cursor-pointer"
              >
                <Plus className="h-3 w-3" />
                Add Provider
              </button>
            </div>

            {/* Provider List */}
            <div className="space-y-2">
              {providersLoading ? (
                <div className="flex items-center justify-center py-8">
                  <Loader className="h-5 w-5 animate-spin text-white/40" />
                </div>
              ) : providers.length === 0 ? (
                <div className="text-center py-8">
                  <div className="flex justify-center mb-3">
                    <div className="flex h-12 w-12 items-center justify-center rounded-xl bg-white/[0.04]">
                      <Cpu className="h-6 w-6 text-white/30" />
                    </div>
                  </div>
                  <p className="text-sm text-white/50 mb-1">No providers configured</p>
                  <p className="text-xs text-white/30">
                    Add an AI provider to enable inference capabilities
                  </p>
                </div>
              ) : (
                providers.map((provider) => {
                  const config = getProviderConfig(provider.provider_type);
                  const statusColor = provider.status.type === 'connected'
                    ? 'bg-emerald-400'
                    : provider.status.type === 'needs_auth'
                    ? 'bg-amber-400'
                    : 'bg-red-400';

                  return (
                    <div
                      key={provider.id}
                      className="group rounded-lg border border-white/[0.06] bg-white/[0.01] hover:bg-white/[0.02] transition-colors"
                    >
                      {editingProvider === provider.id ? (
                        // Edit mode
                        <div className="p-3 space-y-3">
                          <input
                            type="text"
                            value={editForm.name ?? ''}
                            onChange={(e) =>
                              setEditForm({ ...editForm, name: e.target.value })
                            }
                            placeholder="Name"
                            className="w-full rounded-lg border border-white/[0.06] bg-white/[0.02] px-3 py-2 text-sm text-white focus:outline-none focus:border-indigo-500/50"
                          />
                          <input
                            type="password"
                            value={editForm.api_key ?? ''}
                            onChange={(e) =>
                              setEditForm({ ...editForm, api_key: e.target.value })
                            }
                            placeholder="New API key (leave empty to keep)"
                            className="w-full rounded-lg border border-white/[0.06] bg-white/[0.02] px-3 py-2 text-sm text-white focus:outline-none focus:border-indigo-500/50"
                          />
                          {provider.provider_type === 'google' && (
                            <input
                              type="text"
                              value={editForm.google_project_id ?? ''}
                              onChange={(e) =>
                                setEditForm({ ...editForm, google_project_id: e.target.value })
                              }
                              placeholder="Google Cloud project ID (required for Gemini)"
                              className="w-full rounded-lg border border-white/[0.06] bg-white/[0.02] px-3 py-2 text-sm text-white focus:outline-none focus:border-indigo-500/50"
                            />
                          )}
                          <div className="flex items-center justify-between pt-1">
                            <label className="flex items-center gap-2 text-xs text-white/60 cursor-pointer">
                              <input
                                type="checkbox"
                                checked={editForm.enabled ?? true}
                                onChange={(e) =>
                                  setEditForm({ ...editForm, enabled: e.target.checked })
                                }
                                className="rounded border-white/20 cursor-pointer"
                              />
                              Enabled
                            </label>
                            <div className="flex items-center gap-2">
                              <button
                                onClick={handleCancelEdit}
                                className="rounded-lg px-3 py-1.5 text-xs text-white/60 hover:text-white/80 transition-colors cursor-pointer"
                              >
                                Cancel
                              </button>
                              <button
                                onClick={handleSaveEdit}
                                className="rounded-lg bg-indigo-500 px-3 py-1.5 text-xs text-white hover:bg-indigo-600 transition-colors cursor-pointer"
                              >
                                Save
                              </button>
                            </div>
                          </div>
                        </div>
                      ) : (
                        // View mode - minimal single row
                        <div className={cn(
                          'flex items-center gap-3 px-3 py-2.5',
                          !provider.enabled && 'opacity-40'
                        )}>
                          {/* Icon + Name */}
                          <span className="text-base">{config.icon}</span>
                          <span className="text-sm text-white/80 flex-1 truncate">{provider.name}</span>

                          {/* Backend badges */}
                          {provider.use_for_backends && provider.use_for_backends.length > 0 && (
                            <div className="flex items-center gap-1">
                              {provider.use_for_backends.map((backend) => (
                                <span
                                  key={backend}
                                  className="px-1.5 py-0.5 text-[10px] rounded bg-white/[0.06] text-white/50"
                                >
                                  {backend === 'claudecode' ? 'Claude' : backend === 'opencode' ? 'OC' : backend}
                                </span>
                              ))}
                            </div>
                          )}

                          {/* Status indicators */}
                          <div className="flex items-center gap-2">
                            {provider.is_default && (
                              <Star className="h-3 w-3 text-indigo-400 fill-indigo-400" />
                            )}
                            <span className={cn('h-1.5 w-1.5 rounded-full', statusColor)} />
                          </div>

                          {/* Actions on hover */}
                          <div className="flex items-center gap-0.5 opacity-0 group-hover:opacity-100 transition-opacity">
                            {provider.status.type === 'needs_auth' && (
                              <button
                                onClick={() => handleAuthenticate(provider)}
                                disabled={authenticatingProviderId === provider.id}
                                className="p-1.5 rounded-md text-amber-400 hover:bg-white/[0.04] transition-colors cursor-pointer disabled:opacity-50"
                                title="Connect"
                              >
                                {authenticatingProviderId === provider.id ? (
                                  <Loader className="h-3.5 w-3.5 animate-spin" />
                                ) : (
                                  <ExternalLink className="h-3.5 w-3.5" />
                                )}
                              </button>
                            )}
                            {!provider.is_default && provider.enabled && (
                              <button
                                onClick={() => handleSetDefault(provider.id)}
                                className="p-1.5 rounded-md text-white/30 hover:text-white/60 hover:bg-white/[0.04] transition-colors cursor-pointer"
                                title="Set as default"
                              >
                                <Star className="h-3.5 w-3.5" />
                              </button>
                            )}
                            <button
                              onClick={() => handleStartEdit(provider)}
                              className="p-1.5 rounded-md text-white/30 hover:text-white/60 hover:bg-white/[0.04] transition-colors cursor-pointer"
                              title="Edit"
                            >
                              <Key className="h-3.5 w-3.5" />
                            </button>
                            <button
                              onClick={() => handleDeleteProvider(provider.id)}
                              className="p-1.5 rounded-md text-white/30 hover:text-red-400 hover:bg-white/[0.04] transition-colors cursor-pointer"
                              title="Delete"
                            >
                              <Trash2 className="h-3.5 w-3.5" />
                            </button>
                          </div>
                        </div>
                      )}
                    </div>
                  );
                })
              )}
            </div>
          </div>

          {/* Backends */}
          <div className="rounded-xl bg-white/[0.02] border border-white/[0.04] p-5">
            <div className="flex items-center gap-3 mb-4">
              <div className="flex h-10 w-10 items-center justify-center rounded-xl bg-emerald-500/10">
                <Server className="h-5 w-5 text-emerald-400" />
              </div>
              <div>
                <h2 className="text-sm font-medium text-white">Backends</h2>
                <p className="text-xs text-white/40">
                  Configure execution backends and authentication
                </p>
              </div>
            </div>

            <div className="flex items-center gap-2 mb-4">
              {backends.map((backend) => (
                <button
                  key={backend.id}
                  onClick={() =>
                    setActiveBackendTab(
                      backend.id as 'opencode' | 'claudecode' | 'amp'
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
                        ? claudecodeProvider.oauth
                          ? 'OAuth'
                          : claudecodeProvider.api_key
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
                    <button
                      type="button"
                      onClick={() => {
                        document.getElementById('ai-providers')?.scrollIntoView({ behavior: 'smooth' });
                      }}
                      className="text-xs text-amber-400 hover:text-amber-300 transition-colors"
                    >
                      Configure in AI Providers ↑
                    </button>
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
            ) : activeBackendTab === 'amp' ? (
              <div className="space-y-3">
                <label className="flex items-center gap-2 text-xs text-white/60 cursor-pointer">
                  <input
                    type="checkbox"
                    checked={ampForm.enabled}
                    onChange={(e) =>
                      setAmpForm((prev) => ({ ...prev, enabled: e.target.checked }))
                    }
                    className="rounded border-white/20 cursor-pointer"
                  />
                  Enabled
                </label>
                {/* Amp API Key */}
                <div>
                  <label className="block text-xs text-white/60 mb-1.5">
                    <span className="flex items-center gap-1.5">
                      <Key className="h-3.5 w-3.5" />
                      Amp API Key
                    </span>
                  </label>
                  <input
                    type="password"
                    value={ampForm.api_key || ''}
                    onChange={(e) =>
                      setAmpForm((prev) => ({ ...prev, api_key: e.target.value }))
                    }
                    placeholder="Enter your Amp API key from ampcode.com"
                    className="w-full rounded-lg border border-white/[0.06] bg-white/[0.02] px-3 py-2 text-sm text-white font-mono focus:outline-none focus:border-indigo-500/50"
                  />
                  <p className="mt-1.5 text-xs text-white/30">
                    Get your API key from{' '}
                    <a
                      href="https://ampcode.com/settings/tokens"
                      target="_blank"
                      rel="noopener noreferrer"
                      className="text-indigo-400 hover:text-indigo-300"
                    >
                      ampcode.com/settings/tokens
                    </a>
                  </p>
                </div>
                <div>
                  <label className="block text-xs text-white/60 mb-1.5">CLI Path</label>
                  <input
                    type="text"
                    value={ampForm.cli_path || ''}
                    onChange={(e) =>
                      setAmpForm((prev) => ({ ...prev, cli_path: e.target.value }))
                    }
                    placeholder="amp (uses PATH) or /path/to/amp"
                    className="w-full rounded-lg border border-white/[0.06] bg-white/[0.02] px-3 py-2 text-sm text-white focus:outline-none focus:border-indigo-500/50"
                  />
                  <p className="mt-1.5 text-xs text-white/30">
                    Path to the Amp CLI executable. Leave blank to use default from PATH.
                  </p>
                </div>
                <div>
                  <label className="block text-xs text-white/60 mb-1.5">Default Mode</label>
                  <select
                    value={ampForm.default_mode}
                    onChange={(e) =>
                      setAmpForm((prev) => ({ ...prev, default_mode: e.target.value }))
                    }
                    className="w-full rounded-lg border border-white/[0.06] bg-white/[0.02] px-3 py-2 text-sm text-white focus:outline-none focus:border-indigo-500/50"
                  >
                    <option value="smart">Smart Mode (full capability)</option>
                    <option value="rush">Rush Mode (faster, cheaper)</option>
                  </select>
                </div>
                <label className="flex items-center gap-2 text-xs text-white/60 cursor-pointer">
                  <input
                    type="checkbox"
                    checked={ampForm.permissive}
                    onChange={(e) =>
                      setAmpForm((prev) => ({ ...prev, permissive: e.target.checked }))
                    }
                    className="rounded border-white/20 cursor-pointer"
                  />
                  Permissive mode (--dangerously-allow-all)
                </label>
                <div className="flex items-center gap-2 pt-1">
                  <button
                    onClick={handleSaveAmpBackend}
                    disabled={savingBackend}
                    className="flex items-center gap-2 rounded-lg bg-indigo-500 px-3 py-1.5 text-xs text-white hover:bg-indigo-600 transition-colors disabled:opacity-50"
                  >
                    {savingBackend ? (
                      <Loader className="h-3.5 w-3.5 animate-spin" />
                    ) : (
                      <Save className="h-3.5 w-3.5" />
                    )}
                    Save Amp
                  </button>
                  
                </div>
              </div>
            ) : null}
          </div>

          {/* Library Settings */}
          <div className="rounded-xl bg-white/[0.02] border border-white/[0.04] p-5">
            <div className="flex items-center gap-3 mb-4">
              <div className="flex h-10 w-10 items-center justify-center rounded-xl bg-indigo-500/10">
                <GitBranch className="h-5 w-5 text-indigo-400" />
              </div>
              <div>
                <h2 className="text-sm font-medium text-white">Library</h2>
                <p className="text-xs text-white/40">
                  Git-based configuration library for skills, tools, and agents
                </p>
              </div>
            </div>

            <div>
              <label className="block text-xs font-medium text-white/60 mb-1.5">
                Library Remote
              </label>
              {healthLoading ? (
                <div className="flex items-center gap-2 py-2.5">
                  <Loader className="h-4 w-4 animate-spin text-white/40" />
                  <span className="text-sm text-white/40">Loading...</span>
                </div>
              ) : editingLibraryRemote ? (
                <div className="space-y-2">
                  <input
                    type="text"
                    value={libraryRemoteValue}
                    onChange={(e) => setLibraryRemoteValue(e.target.value)}
                    placeholder="git@github.com:your-org/agent-library.git"
                    className="w-full rounded-lg border border-white/[0.06] bg-white/[0.02] px-3 py-2 text-sm text-white font-mono focus:outline-none focus:border-indigo-500/50"
                    autoFocus
                    onKeyDown={(e) => {
                      if (e.key === 'Enter') handleSaveLibraryRemote();
                      if (e.key === 'Escape') handleCancelEditLibraryRemote();
                    }}
                  />
                  <div className="flex items-center gap-2">
                    <button
                      onClick={handleSaveLibraryRemote}
                      disabled={savingLibraryRemote}
                      className="flex items-center gap-1.5 rounded-lg bg-indigo-500 px-3 py-1.5 text-xs text-white hover:bg-indigo-600 transition-colors cursor-pointer disabled:opacity-50"
                    >
                      {savingLibraryRemote ? (
                        <Loader className="h-3 w-3 animate-spin" />
                      ) : (
                        <Check className="h-3 w-3" />
                      )}
                      Save
                    </button>
                    <button
                      onClick={handleCancelEditLibraryRemote}
                      disabled={savingLibraryRemote}
                      className="flex items-center gap-1.5 rounded-lg border border-white/[0.06] px-3 py-1.5 text-xs text-white/60 hover:bg-white/[0.04] transition-colors cursor-pointer disabled:opacity-50"
                    >
                      <X className="h-3 w-3" />
                      Cancel
                    </button>
                  </div>
                </div>
              ) : (
                <div
                  onClick={handleStartEditLibraryRemote}
                  className={cn(
                    'w-full rounded-lg border px-3 py-2.5 text-sm font-mono cursor-pointer transition-colors',
                    serverSettings?.library_remote
                      ? 'border-white/[0.06] bg-white/[0.01] text-white/70 hover:border-indigo-500/30 hover:bg-white/[0.02]'
                      : 'border-amber-500/20 bg-amber-500/5 text-amber-400/80 hover:border-amber-500/30 hover:bg-amber-500/10'
                  )}
                  title="Click to edit"
                >
                  {serverSettings?.library_remote || 'Not configured'}
                </div>
              )}
              <p className="mt-1.5 text-xs text-white/30">
                Git remote URL for skills, tools, agents, and rules. Click to edit.
              </p>
            </div>
          </div>

          {/* Backup & Restore */}
          <div className="rounded-xl bg-white/[0.02] border border-white/[0.04] p-5">
            <div className="flex items-center gap-3 mb-4">
              <div className="flex h-10 w-10 items-center justify-center rounded-xl bg-amber-500/10">
                <Archive className="h-5 w-5 text-amber-400" />
              </div>
              <div>
                <h2 className="text-sm font-medium text-white">Backup & Restore</h2>
                <p className="text-xs text-white/40">
                  Export or import your settings, credentials, and provider configurations
                </p>
              </div>
            </div>

            <div className="space-y-3">
              <p className="text-xs text-white/50">
                Backup includes: AI provider credentials, backend settings (Amp API key, etc.),
                workspace definitions, MCP configurations, encrypted secrets, and the
                library encryption key.
              </p>

              <div className="flex items-center gap-3">
                <button
                  onClick={handleDownloadBackup}
                  disabled={downloadingBackup}
                  className="flex items-center gap-2 rounded-lg bg-indigo-500 px-4 py-2 text-sm text-white hover:bg-indigo-600 transition-colors disabled:opacity-50"
                >
                  {downloadingBackup ? (
                    <Loader className="h-4 w-4 animate-spin" />
                  ) : (
                    <Download className="h-4 w-4" />
                  )}
                  Download Backup
                </button>

                <label className="flex items-center gap-2 rounded-lg border border-white/[0.08] bg-white/[0.02] px-4 py-2 text-sm text-white/70 hover:bg-white/[0.04] transition-colors cursor-pointer">
                  {restoringBackup ? (
                    <Loader className="h-4 w-4 animate-spin" />
                  ) : (
                    <Upload className="h-4 w-4" />
                  )}
                  Restore Backup
                  <input
                    type="file"
                    accept=".zip"
                    className="hidden"
                    ref={fileInputRef}
                    disabled={restoringBackup}
                    onChange={(e) => {
                      const file = e.target.files?.[0];
                      if (file) {
                        handleRestoreBackup(file);
                        e.target.value = '';
                      }
                    }}
                  />
                </label>
              </div>

              <p className="text-xs text-white/30">
                After restoring, a server restart may be required to apply credential changes.
              </p>
            </div>
          </div>

        </div>
      </div>
    </div>
  );
}
