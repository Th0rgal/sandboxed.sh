'use client';

import { useState, useEffect, useCallback } from 'react';
import { toast } from 'sonner';
import {
  getHealth,
  HealthResponse,
  listAIProviders,
  listAIProviderTypes,
  createAIProvider,
  updateAIProvider,
  deleteAIProvider,
  authenticateAIProvider,
  setDefaultAIProvider,
  AIProvider,
  AIProviderType,
  AIProviderTypeInfo,
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
  Check,
  X,
  Star,
  ExternalLink,
  Loader,
  Key,
  Link2,
  Shield,
  ChevronDown,
} from 'lucide-react';
import { readSavedSettings, writeSavedSettings } from '@/lib/settings';
import { cn } from '@/lib/utils';

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
  'github-copilot': { color: 'bg-gray-500/10 text-gray-400', icon: '🐙' },
  custom: { color: 'bg-white/10 text-white/60', icon: '🔧' },
};

function getProviderConfig(type: string) {
  return providerConfig[type] || providerConfig.custom;
}

export default function SettingsPage() {
  const [health, setHealth] = useState<HealthResponse | null>(null);
  const [healthLoading, setHealthLoading] = useState(true);
  const [testingConnection, setTestingConnection] = useState(false);

  // Form state
  const [apiUrl, setApiUrl] = useState(
    () => readSavedSettings().apiUrl ?? 'http://127.0.0.1:3000'
  );
  const [libraryRepo, setLibraryRepo] = useState(
    () => readSavedSettings().libraryRepo ?? ''
  );

  // Track original values for unsaved changes
  const [originalValues, setOriginalValues] = useState({
    apiUrl: readSavedSettings().apiUrl ?? 'http://127.0.0.1:3000',
    libraryRepo: readSavedSettings().libraryRepo ?? '',
  });

  // Validation state
  const [urlError, setUrlError] = useState<string | null>(null);
  const [repoError, setRepoError] = useState<string | null>(null);

  // AI Providers state
  const [providers, setProviders] = useState<AIProvider[]>([]);
  const [providerTypes, setProviderTypes] = useState<AIProviderTypeInfo[]>([]);
  const [providersLoading, setProvidersLoading] = useState(true);
  const [showNewProvider, setShowNewProvider] = useState(false);
  const [newProvider, setNewProvider] = useState({
    provider_type: 'anthropic' as AIProviderType,
    name: '',
    api_key: '',
    base_url: '',
  });
  const [savingProvider, setSavingProvider] = useState(false);
  const [authenticatingProviderId, setAuthenticatingProviderId] = useState<string | null>(null);
  const [editingProvider, setEditingProvider] = useState<string | null>(null);
  const [editForm, setEditForm] = useState<{
    name?: string;
    api_key?: string;
    base_url?: string;
    enabled?: boolean;
  }>({});

  // Check if there are unsaved changes
  const hasUnsavedChanges =
    apiUrl !== originalValues.apiUrl || libraryRepo !== originalValues.libraryRepo;

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

  const validateRepo = useCallback((repo: string) => {
    const trimmed = repo.trim();
    if (!trimmed) {
      setRepoError(null);
      return true;
    }
    if (/\s/.test(trimmed)) {
      setRepoError('Repository URL cannot contain spaces');
      return false;
    }
    setRepoError(null);
    return true;
  }, []);

  // Load health and providers on mount
  useEffect(() => {
    const checkHealth = async () => {
      setHealthLoading(true);
      try {
        const data = await getHealth();
        setHealth(data);
      } catch {
        setHealth(null);
      } finally {
        setHealthLoading(false);
      }
    };
    checkHealth();
    loadProviders();
    loadProviderTypes();
  }, []);

  const loadProviders = async () => {
    try {
      setProvidersLoading(true);
      const data = await listAIProviders();
      setProviders(data);
    } catch {
      // Silent fail - providers might not be available yet
    } finally {
      setProvidersLoading(false);
    }
  };

  const loadProviderTypes = async () => {
    try {
      const data = await listAIProviderTypes();
      setProviderTypes(data);
    } catch {
      // Use defaults if API fails
      setProviderTypes([
        { id: 'anthropic', name: 'Anthropic', uses_oauth: true, env_var: 'ANTHROPIC_API_KEY' },
        { id: 'openai', name: 'OpenAI', uses_oauth: false, env_var: 'OPENAI_API_KEY' },
        { id: 'google', name: 'Google AI', uses_oauth: false, env_var: 'GOOGLE_API_KEY' },
        { id: 'open-router', name: 'OpenRouter', uses_oauth: false, env_var: 'OPENROUTER_API_KEY' },
        { id: 'groq', name: 'Groq', uses_oauth: false, env_var: 'GROQ_API_KEY' },
        { id: 'mistral', name: 'Mistral AI', uses_oauth: false, env_var: 'MISTRAL_API_KEY' },
        { id: 'xai', name: 'xAI', uses_oauth: false, env_var: 'XAI_API_KEY' },
        { id: 'github-copilot', name: 'GitHub Copilot', uses_oauth: true, env_var: null },
      ]);
    }
  };

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
  }, [apiUrl, libraryRepo]);

  const handleSave = () => {
    const urlValid = validateUrl(apiUrl);
    const repoValid = validateRepo(libraryRepo);

    if (!urlValid || !repoValid) {
      toast.error('Please fix validation errors before saving');
      return;
    }

    writeSavedSettings({ apiUrl, libraryRepo });
    setOriginalValues({ apiUrl, libraryRepo });
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
      setHealth(data);
      toast.success(`Connected to OpenAgent v${data.version}`);
    } catch (err) {
      setHealth(null);
      toast.error(
        `Connection failed: ${err instanceof Error ? err.message : 'Unknown error'}`
      );
    } finally {
      setTestingConnection(false);
    }
  };

  const handleCreateProvider = async () => {
    if (!newProvider.name.trim()) {
      toast.error('Name is required');
      return;
    }

    const typeInfo = providerTypes.find((t) => t.id === newProvider.provider_type);
    const needsApiKey = !typeInfo?.uses_oauth;

    if (needsApiKey && !newProvider.api_key.trim()) {
      toast.error('API key is required for this provider');
      return;
    }

    if (newProvider.base_url) {
      try {
        new URL(newProvider.base_url);
      } catch {
        toast.error('Invalid base URL format');
        return;
      }
    }

    setSavingProvider(true);
    try {
      await createAIProvider({
        provider_type: newProvider.provider_type,
        name: newProvider.name,
        api_key: newProvider.api_key || undefined,
        base_url: newProvider.base_url || undefined,
      });
      toast.success('Provider added');
      setShowNewProvider(false);
      setNewProvider({
        provider_type: 'anthropic',
        name: '',
        api_key: '',
        base_url: '',
      });
      loadProviders();
    } catch (err) {
      toast.error(
        `Failed to create provider: ${err instanceof Error ? err.message : 'Unknown error'}`
      );
    } finally {
      setSavingProvider(false);
    }
  };

  const handleAuthenticate = async (provider: AIProvider) => {
    setAuthenticatingProviderId(provider.id);
    try {
      const result = await authenticateAIProvider(provider.id);
      if (result.success) {
        toast.success(result.message);
        loadProviders();
      } else {
        if (result.auth_url) {
          // Open auth URL in new window
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
      loadProviders();
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
      loadProviders();
    } catch (err) {
      toast.error(
        `Failed to delete: ${err instanceof Error ? err.message : 'Unknown error'}`
      );
    }
  };

  const handleStartEdit = (provider: AIProvider) => {
    setEditingProvider(provider.id);
    setEditForm({
      name: provider.name,
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
        api_key: editForm.api_key || undefined,
        base_url: editForm.base_url || undefined,
        enabled: editForm.enabled,
      });
      toast.success('Provider updated');
      setEditingProvider(null);
      loadProviders();
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

  const getStatusBadge = (provider: AIProvider) => {
    switch (provider.status.type) {
      case 'connected':
        return (
          <span className="flex items-center gap-1 rounded-full bg-emerald-500/20 px-2 py-0.5 text-[10px] font-medium text-emerald-400">
            <span className="h-1.5 w-1.5 rounded-full bg-emerald-400" />
            Connected
          </span>
        );
      case 'needs_auth':
        return (
          <span className="flex items-center gap-1 rounded-full bg-amber-500/20 px-2 py-0.5 text-[10px] font-medium text-amber-400">
            <Key className="h-2.5 w-2.5" />
            Needs Auth
          </span>
        );
      case 'error':
        return (
          <span className="flex items-center gap-1 rounded-full bg-red-500/20 px-2 py-0.5 text-[10px] font-medium text-red-400">
            <AlertTriangle className="h-2.5 w-2.5" />
            Error
          </span>
        );
      default:
        return (
          <span className="flex items-center gap-1 rounded-full bg-white/10 px-2 py-0.5 text-[10px] text-white/40">
            Unknown
          </span>
        );
    }
  };

  const selectedTypeInfo = providerTypes.find((t) => t.id === newProvider.provider_type);

  return (
    <div className="min-h-screen flex flex-col items-center p-6">
      {/* Centered content container */}
      <div className="w-full max-w-xl">
        {/* Header */}
        <div className="mb-8 flex items-start justify-between">
          <div>
            <h1 className="text-xl font-semibold text-white">Settings</h1>
            <p className="mt-1 text-sm text-white/50">
              Configure your server connection and AI providers
            </p>
          </div>
          {hasUnsavedChanges && (
            <div className="flex items-center gap-2 text-amber-400 text-xs">
              <AlertTriangle className="h-3.5 w-3.5" />
              <span>Unsaved changes</span>
            </div>
          )}
        </div>

        <div className="space-y-5">
          {/* API Connection */}
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
                  onChange={(e) => {
                    setApiUrl(e.target.value);
                    validateUrl(e.target.value);
                  }}
                  className={cn(
                    'w-full rounded-lg border bg-white/[0.02] px-3 py-2.5 text-sm text-white placeholder-white/30 focus:outline-none transition-colors',
                    urlError
                      ? 'border-red-500/50 focus:border-red-500/50'
                      : 'border-white/[0.06] focus:border-indigo-500/50'
                  )}
                />
                {urlError && <p className="mt-1.5 text-xs text-red-400">{urlError}</p>}
              </div>

              <div className="flex items-center justify-between">
                <div className="flex items-center gap-2">
                  <span className="text-xs text-white/40">Status:</span>
                  {healthLoading ? (
                    <span className="flex items-center gap-1.5 text-xs text-white/40">
                      <RefreshCw className="h-3 w-3 animate-spin" />
                      Checking...
                    </span>
                  ) : health ? (
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

                <button
                  onClick={testApiConnection}
                  disabled={testingConnection}
                  className="flex items-center gap-2 rounded-lg border border-white/[0.06] bg-white/[0.02] px-3 py-1.5 text-xs text-white/70 hover:bg-white/[0.04] transition-colors disabled:opacity-50"
                >
                  <RefreshCw
                    className={cn('h-3 w-3', testingConnection && 'animate-spin')}
                  />
                  Test Connection
                </button>
              </div>
            </div>
          </div>

          {/* AI Providers */}
          <div className="rounded-xl bg-white/[0.02] border border-white/[0.04] p-5">
            <div className="flex items-center justify-between mb-4">
              <div className="flex items-center gap-3">
                <div className="flex h-10 w-10 items-center justify-center rounded-xl bg-violet-500/10">
                  <Cpu className="h-5 w-5 text-violet-400" />
                </div>
                <div>
                  <h2 className="text-sm font-medium text-white">AI Providers</h2>
                  <p className="text-xs text-white/40">
                    Configure inference providers for OpenCode
                  </p>
                </div>
              </div>
              <button
                onClick={() => setShowNewProvider(true)}
                className="flex items-center gap-1.5 rounded-lg border border-white/[0.06] bg-white/[0.02] px-3 py-1.5 text-xs text-white/70 hover:bg-white/[0.04] transition-colors"
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
                  return (
                    <div
                      key={provider.id}
                      className={cn(
                        'rounded-lg border p-3 transition-colors',
                        provider.is_default
                          ? 'border-violet-500/30 bg-violet-500/5'
                          : 'border-white/[0.06] bg-white/[0.01]'
                      )}
                    >
                      {editingProvider === provider.id ? (
                        // Edit mode
                        <div className="space-y-3">
                          <input
                            type="text"
                            value={editForm.name ?? ''}
                            onChange={(e) =>
                              setEditForm({ ...editForm, name: e.target.value })
                            }
                            placeholder="Name"
                            className="w-full rounded-lg border border-white/[0.06] bg-white/[0.02] px-3 py-2 text-sm text-white focus:outline-none focus:border-violet-500/50"
                          />
                          <div>
                            <label className="block text-xs text-white/40 mb-1">
                              API Key (leave empty to keep current)
                            </label>
                            <input
                              type="password"
                              value={editForm.api_key ?? ''}
                              onChange={(e) =>
                                setEditForm({ ...editForm, api_key: e.target.value })
                              }
                              placeholder="••••••••••••••••"
                              className="w-full rounded-lg border border-white/[0.06] bg-white/[0.02] px-3 py-2 text-sm text-white focus:outline-none focus:border-violet-500/50"
                            />
                          </div>
                          <input
                            type="text"
                            value={editForm.base_url ?? ''}
                            onChange={(e) =>
                              setEditForm({ ...editForm, base_url: e.target.value })
                            }
                            placeholder="Custom base URL (optional)"
                            className="w-full rounded-lg border border-white/[0.06] bg-white/[0.02] px-3 py-2 text-sm text-white focus:outline-none focus:border-violet-500/50"
                          />
                          <label className="flex items-center gap-2 text-xs text-white/60">
                            <input
                              type="checkbox"
                              checked={editForm.enabled ?? true}
                              onChange={(e) =>
                                setEditForm({ ...editForm, enabled: e.target.checked })
                              }
                              className="rounded border-white/20"
                            />
                            Enabled
                          </label>
                          <div className="flex items-center gap-2 pt-1">
                            <button
                              onClick={handleSaveEdit}
                              className="flex items-center gap-1.5 rounded-lg bg-violet-500 px-3 py-1.5 text-xs text-white hover:bg-violet-600 transition-colors"
                            >
                              <Check className="h-3 w-3" />
                              Save
                            </button>
                            <button
                              onClick={handleCancelEdit}
                              className="flex items-center gap-1.5 rounded-lg border border-white/[0.06] px-3 py-1.5 text-xs text-white/70 hover:bg-white/[0.04] transition-colors"
                            >
                              <X className="h-3 w-3" />
                              Cancel
                            </button>
                          </div>
                        </div>
                      ) : (
                        // View mode
                        <div>
                          <div className="flex items-start justify-between">
                            <div className="flex items-start gap-3">
                              <div
                                className={cn(
                                  'flex h-9 w-9 items-center justify-center rounded-lg text-lg',
                                  config.color
                                )}
                              >
                                {config.icon}
                              </div>
                              <div className="min-w-0 flex-1">
                                <div className="flex items-center gap-2 flex-wrap">
                                  <h3 className="text-sm font-medium text-white">
                                    {provider.name}
                                  </h3>
                                  {provider.is_default && (
                                    <span className="flex items-center gap-1 rounded-full bg-violet-500/20 px-2 py-0.5 text-[10px] font-medium text-violet-400">
                                      <Star className="h-2.5 w-2.5" />
                                      Default
                                    </span>
                                  )}
                                  {!provider.enabled && (
                                    <span className="rounded-full bg-white/10 px-2 py-0.5 text-[10px] text-white/40">
                                      Disabled
                                    </span>
                                  )}
                                </div>
                                <p className="text-xs text-white/40 mt-0.5">
                                  {provider.provider_type_name}
                                </p>
                                <div className="flex items-center gap-2 mt-1.5">
                                  {getStatusBadge(provider)}
                                  {provider.has_api_key && (
                                    <span className="flex items-center gap-1 text-[10px] text-white/30">
                                      <Key className="h-2.5 w-2.5" />
                                      API key set
                                    </span>
                                  )}
                                  {provider.base_url && (
                                    <span className="flex items-center gap-1 text-[10px] text-white/30">
                                      <Link2 className="h-2.5 w-2.5" />
                                      Custom URL
                                    </span>
                                  )}
                                </div>
                              </div>
                            </div>
                          </div>
                          <div className="flex items-center gap-2 mt-3 ml-12">
                            {provider.status.type === 'needs_auth' && (
                              <button
                                onClick={() => handleAuthenticate(provider)}
                                disabled={authenticatingProviderId === provider.id}
                                className="flex items-center gap-1.5 rounded-lg bg-amber-500/20 border border-amber-500/30 px-2.5 py-1 text-xs text-amber-400 hover:bg-amber-500/30 transition-colors disabled:opacity-50"
                              >
                                {authenticatingProviderId === provider.id ? (
                                  <Loader className="h-3 w-3 animate-spin" />
                                ) : (
                                  <ExternalLink className="h-3 w-3" />
                                )}
                                Connect
                              </button>
                            )}
                            {!provider.is_default && provider.enabled && (
                              <button
                                onClick={() => handleSetDefault(provider.id)}
                                className="flex items-center gap-1.5 rounded-lg border border-white/[0.06] bg-white/[0.02] px-2.5 py-1 text-xs text-white/60 hover:bg-white/[0.04] transition-colors"
                              >
                                <Star className="h-3 w-3" />
                                Set Default
                              </button>
                            )}
                            <button
                              onClick={() => handleStartEdit(provider)}
                              className="flex items-center gap-1.5 rounded-lg border border-white/[0.06] bg-white/[0.02] px-2.5 py-1 text-xs text-white/60 hover:bg-white/[0.04] transition-colors"
                            >
                              Edit
                            </button>
                            <button
                              onClick={() => handleDeleteProvider(provider.id)}
                              className="flex items-center gap-1.5 rounded-lg border border-red-500/20 bg-red-500/5 px-2.5 py-1 text-xs text-red-400 hover:bg-red-500/10 transition-colors"
                            >
                              <Trash2 className="h-3 w-3" />
                            </button>
                          </div>
                        </div>
                      )}
                    </div>
                  );
                })
              )}
            </div>

            {/* New Provider Form */}
            {showNewProvider && (
              <div className="mt-4 rounded-lg border border-violet-500/30 bg-violet-500/5 p-4">
                <h3 className="text-sm font-medium text-white mb-3">Add AI Provider</h3>
                <div className="space-y-3">
                  <div>
                    <label className="block text-xs font-medium text-white/60 mb-1">
                      Provider Type
                    </label>
                    <div className="relative">
                      <select
                        value={newProvider.provider_type}
                        onChange={(e) => {
                          const type = e.target.value as AIProviderType;
                          const typeInfo = providerTypes.find((t) => t.id === type);
                          setNewProvider({
                            ...newProvider,
                            provider_type: type,
                            name: typeInfo?.name || type,
                          });
                        }}
                        className="w-full appearance-none rounded-lg border border-white/[0.06] bg-white/[0.02] px-3 py-2 text-sm text-white focus:outline-none focus:border-violet-500/50 cursor-pointer"
                      >
                        {providerTypes.map((type) => (
                          <option key={type.id} value={type.id} className="bg-[#1a1a1c]">
                            {type.name}
                          </option>
                        ))}
                      </select>
                      <ChevronDown className="absolute right-3 top-1/2 -translate-y-1/2 h-4 w-4 text-white/40 pointer-events-none" />
                    </div>
                  </div>
                  <div>
                    <label className="block text-xs font-medium text-white/60 mb-1">
                      Display Name
                    </label>
                    <input
                      type="text"
                      value={newProvider.name}
                      onChange={(e) =>
                        setNewProvider({ ...newProvider, name: e.target.value })
                      }
                      placeholder="e.g., My Claude Account"
                      className="w-full rounded-lg border border-white/[0.06] bg-white/[0.02] px-3 py-2 text-sm text-white placeholder-white/30 focus:outline-none focus:border-violet-500/50"
                    />
                  </div>
                  {!selectedTypeInfo?.uses_oauth && (
                    <div>
                      <label className="block text-xs font-medium text-white/60 mb-1">
                        API Key
                        {selectedTypeInfo?.env_var && (
                          <span className="ml-2 text-white/30 font-normal">
                            ({selectedTypeInfo.env_var})
                          </span>
                        )}
                      </label>
                      <input
                        type="password"
                        value={newProvider.api_key}
                        onChange={(e) =>
                          setNewProvider({ ...newProvider, api_key: e.target.value })
                        }
                        placeholder="sk-..."
                        className="w-full rounded-lg border border-white/[0.06] bg-white/[0.02] px-3 py-2 text-sm text-white placeholder-white/30 focus:outline-none focus:border-violet-500/50"
                      />
                    </div>
                  )}
                  {selectedTypeInfo?.uses_oauth && (
                    <div className="flex items-start gap-2 rounded-lg bg-amber-500/10 border border-amber-500/20 p-3">
                      <Shield className="h-4 w-4 text-amber-400 mt-0.5 shrink-0" />
                      <p className="text-xs text-amber-300">
                        This provider uses OAuth authentication. After adding, click
                        &quot;Connect&quot; to authenticate with {selectedTypeInfo.name}.
                      </p>
                    </div>
                  )}
                  <div>
                    <label className="block text-xs font-medium text-white/60 mb-1">
                      Custom Base URL (optional)
                    </label>
                    <input
                      type="text"
                      value={newProvider.base_url}
                      onChange={(e) =>
                        setNewProvider({ ...newProvider, base_url: e.target.value })
                      }
                      placeholder="https://api.example.com/v1"
                      className="w-full rounded-lg border border-white/[0.06] bg-white/[0.02] px-3 py-2 text-sm text-white placeholder-white/30 focus:outline-none focus:border-violet-500/50"
                    />
                    <p className="mt-1 text-xs text-white/30">
                      Override the default API endpoint (for proxies or self-hosted)
                    </p>
                  </div>
                  <div className="flex items-center gap-2 pt-1">
                    <button
                      onClick={handleCreateProvider}
                      disabled={savingProvider}
                      className="flex items-center gap-1.5 rounded-lg bg-violet-500 px-3 py-1.5 text-xs text-white hover:bg-violet-600 transition-colors disabled:opacity-50"
                    >
                      {savingProvider ? (
                        <Loader className="h-3 w-3 animate-spin" />
                      ) : (
                        <Plus className="h-3 w-3" />
                      )}
                      Add Provider
                    </button>
                    <button
                      onClick={() => setShowNewProvider(false)}
                      className="flex items-center gap-1.5 rounded-lg border border-white/[0.06] px-3 py-1.5 text-xs text-white/70 hover:bg-white/[0.04] transition-colors"
                    >
                      Cancel
                    </button>
                  </div>
                </div>
              </div>
            )}
          </div>

          {/* Configuration Library */}
          <div className="rounded-xl bg-white/[0.02] border border-white/[0.04] p-5">
            <div className="flex items-center gap-3 mb-4">
              <div className="flex h-10 w-10 items-center justify-center rounded-xl bg-indigo-500/10">
                <GitBranch className="h-5 w-5 text-indigo-400" />
              </div>
              <div>
                <h2 className="text-sm font-medium text-white">Configuration Library</h2>
                <p className="text-xs text-white/40">
                  Git repo for MCPs, skills, and commands
                </p>
              </div>
            </div>

            <div>
              <label className="block text-xs font-medium text-white/60 mb-1.5">
                Library Repo (optional)
              </label>
              <input
                type="text"
                value={libraryRepo}
                onChange={(e) => {
                  setLibraryRepo(e.target.value);
                  validateRepo(e.target.value);
                }}
                placeholder="https://github.com/your/library.git"
                className={cn(
                  'w-full rounded-lg border bg-white/[0.02] px-3 py-2.5 text-sm text-white placeholder-white/30 focus:outline-none transition-colors',
                  repoError
                    ? 'border-red-500/50 focus:border-red-500/50'
                    : 'border-white/[0.06] focus:border-indigo-500/50'
                )}
              />
              {repoError ? (
                <p className="mt-1.5 text-xs text-red-400">{repoError}</p>
              ) : (
                <p className="mt-1.5 text-xs text-white/30">
                  Leave blank to disable library features.
                </p>
              )}
            </div>
          </div>

          {/* Save Button */}
          <button
            onClick={handleSave}
            disabled={!!urlError || !!repoError}
            className={cn(
              'w-full flex items-center justify-center gap-2 rounded-lg px-4 py-2.5 text-sm font-medium text-white transition-colors',
              urlError || repoError
                ? 'bg-white/10 cursor-not-allowed opacity-50'
                : 'bg-indigo-500 hover:bg-indigo-600'
            )}
          >
            <Save className="h-4 w-4" />
            Save Settings
            <span className="text-xs text-white/50 ml-1">(⌘S)</span>
          </button>
        </div>
      </div>
    </div>
  );
}
