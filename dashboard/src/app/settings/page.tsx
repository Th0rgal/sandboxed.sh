'use client';

import { useState, useEffect, useCallback } from 'react';
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
    api_key?: string;
    base_url?: string;
    enabled?: boolean;
  }>({});

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

          {/* Library Settings */}
          <div className="rounded-xl bg-white/[0.02] border border-white/[0.04] p-5">
            <div className="flex items-center gap-3 mb-4">
              <div className="flex h-10 w-10 items-center justify-center rounded-xl bg-indigo-500/10">
                <GitBranch className="h-5 w-5 text-indigo-400" />
              </div>
              <div>
                <h2 className="text-sm font-medium text-white">Library</h2>
                <p className="text-xs text-white/40">
                  Configuration library (server-managed)
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
              ) : health?.library_remote ? (
                <div className="w-full rounded-lg border border-white/[0.06] bg-white/[0.01] px-3 py-2.5 text-sm text-white/70 font-mono">
                  {health.library_remote}
                </div>
              ) : (
                <div className="w-full rounded-lg border border-amber-500/20 bg-amber-500/5 px-3 py-2.5 text-sm text-amber-400/80">
                  Not configured
                </div>
              )}
              <p className="mt-1.5 text-xs text-white/30">
                Set via <code className="text-white/50">LIBRARY_REMOTE</code> environment variable on the server.
              </p>
            </div>
          </div>

        </div>
      </div>
    </div>
  );
}
