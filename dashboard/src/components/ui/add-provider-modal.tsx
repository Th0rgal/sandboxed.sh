'use client';

import { useState, useRef, useEffect } from 'react';
import { X, ExternalLink, Key, Loader, Cpu, Plus, Trash2 } from 'lucide-react';
import { toast } from '@/components/toast';
import { cn } from '@/lib/utils';
import {
  createAIProvider,
  oauthAuthorize,
  oauthCallback,
  AIProviderType,
  AIProviderTypeInfo,
  AIProviderAuthMethod,
  OAuthAuthorizeResponse,
  CustomModel,
} from '@/lib/api';

// Provider icons mapping
const providerIcons: Record<string, string> = {
  anthropic: '🧠',
  openai: '🤖',
  google: '🔮',
  'deep-infra': '🚀',
  cerebras: '🧬',
  cohere: '🌊',
  'together-ai': '🤝',
  perplexity: '🔍',
  zai: '⚡',
  minimax: 'M',
  amp: 'A',
  custom: '🔧',
};

interface AddProviderModalProps {
  open: boolean;
  onClose: () => void;
  onSuccess: () => void;
  providerTypes: AIProviderTypeInfo[];
}

// Get auth methods for a provider type
const getProviderAuthMethods = (providerType: AIProviderType): AIProviderAuthMethod[] => {
  if (providerType === 'anthropic') {
    return [
      { label: 'Claude Pro/Max', type: 'oauth', description: 'Use your Claude subscription' },
      { label: 'Create API Key', type: 'oauth', description: 'Create a new key via OAuth' },
      { label: 'Enter API Key', type: 'api', description: 'Use an existing API key' },
    ];
  }
  if (providerType === 'openai') {
    return [
      {
        label: 'ChatGPT Plus/Pro (OAuth)',
        type: 'oauth',
        description: 'Use your ChatGPT subscription via official OAuth',
      },
      { label: 'Enter API Key', type: 'api', description: 'Use an existing API key' },
    ];
  }
  if (providerType === 'google') {
    return [
      {
        label: 'OAuth with Google (Gemini CLI)',
        type: 'oauth',
        description: 'Use your Gemini plan/quotas (including free tier) via Google OAuth',
      },
      { label: 'Enter API Key', type: 'api', description: 'Use an existing Google AI API key' },
    ];
  }
  return [];
};

type ModalStep = 'select-provider' | 'select-method' | 'select-backends' | 'enter-api-key' | 'oauth-callback' | 'custom-provider';

export function AddProviderModal({ open, onClose, onSuccess, providerTypes }: AddProviderModalProps) {
  const dialogRef = useRef<HTMLDivElement>(null);

  // State
  const [step, setStep] = useState<ModalStep>('select-provider');
  const [selectedProvider, setSelectedProvider] = useState<AIProviderType | null>(null);
  const [selectedMethodIndex, setSelectedMethodIndex] = useState<number | null>(null);
  const [apiKey, setApiKey] = useState('');
  const [accountLabel, setAccountLabel] = useState('');
  const [oauthResponse, setOauthResponse] = useState<OAuthAuthorizeResponse | null>(null);
  const [oauthCode, setOauthCode] = useState('');
  const [loading, setLoading] = useState(false);
  // Backend selection for providers that can target multiple harness backends.
  const [selectedBackends, setSelectedBackends] = useState<string[]>(['opencode']);

  // Custom provider state
  const [customName, setCustomName] = useState('');
  const [customBaseUrl, setCustomBaseUrl] = useState('');
  const [customApiKey, setCustomApiKey] = useState('');
  const [customEnvVar, setCustomEnvVar] = useState('');
  const [customModels, setCustomModels] = useState<CustomModel[]>([{ id: '', name: '' }]);

  // Get selected provider info
  const selectedTypeInfo = selectedProvider ? providerTypes.find(t => t.id === selectedProvider) : null;
  const authMethods = selectedProvider ? getProviderAuthMethods(selectedProvider) : [];
  const hasOAuth = selectedTypeInfo?.uses_oauth && authMethods.length > 0;

  // Reset state when modal opens/closes
  useEffect(() => {
    if (open) {
      setStep('select-provider');
      setSelectedProvider(null);
      setSelectedMethodIndex(null);
      setApiKey('');
      setAccountLabel('');
      setOauthResponse(null);
      setOauthCode('');
      setLoading(false);
      setSelectedBackends(['opencode']);
      // Reset custom provider state
      setCustomName('');
      setCustomBaseUrl('');
      setCustomApiKey('');
      setCustomEnvVar('');
      setCustomModels([{ id: '', name: '' }]);
    }
  }, [open]);

  // Handle escape key
  useEffect(() => {
    const handleKeyDown = (e: KeyboardEvent) => {
      if (e.key === 'Escape' && open && !loading) {
        handleClose();
      }
    };
    document.addEventListener('keydown', handleKeyDown);
    return () => document.removeEventListener('keydown', handleKeyDown);
  }, [open, loading]);

  // Handle click outside
  useEffect(() => {
    const handleClickOutside = (e: MouseEvent) => {
      if (dialogRef.current && !dialogRef.current.contains(e.target as Node) && !loading) {
        handleClose();
      }
    };
    if (open) {
      document.addEventListener('mousedown', handleClickOutside);
      return () => document.removeEventListener('mousedown', handleClickOutside);
    }
  }, [open, loading]);

  const handleClose = async () => {
    onClose();
  };

  const handleSelectProvider = (providerType: AIProviderType) => {
    setSelectedProvider(providerType);

    // Custom provider has its own flow
    if (providerType === 'custom') {
      setStep('custom-provider');
      return;
    }

    const typeInfo = providerTypes.find(t => t.id === providerType);
    const methods = getProviderAuthMethods(providerType);

    // Default backend targeting by provider (may be adjusted after method selection).
    if (providerType === 'anthropic') {
      setSelectedBackends(['opencode']);
    } else if (providerType === 'openai') {
      setSelectedBackends(['opencode', 'codex']);
    } else if (providerType === 'amp') {
      setSelectedBackends(['amp']);
    } else {
      setSelectedBackends(['opencode']);
    }

    // If provider has OAuth options, show method selection
    if (typeInfo?.uses_oauth && methods.length > 0) {
      setStep('select-method');
    } else {
      // Otherwise go directly to API key entry
      setStep('enter-api-key');
    }
  };

  const handleSelectMethod = async (methodIndex: number) => {
    const method = authMethods[methodIndex];
    setSelectedMethodIndex(methodIndex);

    // Providers that support multiple backends should select targeting first.
    if (selectedProvider === 'anthropic' || selectedProvider === 'openai') {
      // For OpenAI, default backends depend on auth method.
      if (selectedProvider === 'openai') {
        setSelectedBackends(['opencode', 'codex']);
      }
      setStep('select-backends');
      return;
    }

    if (method.type === 'api') {
      setStep('enter-api-key');
    } else {
      // Start OAuth flow
      setLoading(true);
      try {
        const response = await oauthAuthorize(selectedProvider!, methodIndex);
        setOauthResponse(response);
        setStep('oauth-callback');
        window.open(response.url, '_blank');
      } catch (err) {
        toast.error(`Failed: ${err instanceof Error ? err.message : 'Unknown error'}`);
      } finally {
        setLoading(false);
      }
    }
  };

  const handleContinueFromBackends = async () => {
    if (selectedBackends.length === 0) {
      toast.error('Please select at least one backend');
      return;
    }

    const method = authMethods[selectedMethodIndex!];
    if (method.type === 'api') {
      setStep('enter-api-key');
    } else {
      // Start OAuth flow
      setLoading(true);
      try {
        const response = await oauthAuthorize(selectedProvider!, selectedMethodIndex!);
        setOauthResponse(response);
        setStep('oauth-callback');
        window.open(response.url, '_blank');
      } catch (err) {
        toast.error(`Failed: ${err instanceof Error ? err.message : 'Unknown error'}`);
      } finally {
        setLoading(false);
      }
    }
  };

  const toggleBackend = (backendId: string) => {
    setSelectedBackends(prev =>
      prev.includes(backendId)
        ? prev.filter(b => b !== backendId)
        : [...prev, backendId]
    );
  };

  const handleSubmitApiKey = async () => {
    if (!apiKey.trim() || !selectedProvider) return;

    setLoading(true);
    try {
      await createAIProvider({
        provider_type: selectedProvider,
        name: accountLabel.trim()
          ? `${selectedTypeInfo?.name || selectedProvider} (${accountLabel.trim()})`
          : selectedTypeInfo?.name || selectedProvider,
        api_key: apiKey,
        use_for_backends: selectedBackends,
      });
      toast.success('Provider added');
      onSuccess();
      onClose();
    } catch (err) {
      toast.error(`Failed: ${err instanceof Error ? err.message : 'Unknown error'}`);
    } finally {
      setLoading(false);
    }
  };

  const handleSubmitOAuthCode = async () => {
    if (!oauthCode.trim() || !selectedProvider || selectedMethodIndex === null) return;

    setLoading(true);
    try {
      await oauthCallback(
        selectedProvider,
        selectedMethodIndex,
        oauthCode,
        // Include backend targeting for supported providers
        selectedProvider === 'anthropic' || selectedProvider === 'openai'
          ? selectedBackends
          : undefined
      );
      toast.success('Provider connected');
      onSuccess();
      onClose();
    } catch (err) {
      toast.error(`Failed: ${err instanceof Error ? err.message : 'Unknown error'}`);
    } finally {
      setLoading(false);
    }
  };

  // Custom provider model management
  const handleAddModel = () => {
    setCustomModels([...customModels, { id: '', name: '' }]);
  };

  const handleRemoveModel = (index: number) => {
    if (customModels.length > 1) {
      setCustomModels(customModels.filter((_, i) => i !== index));
    }
  };

  const handleUpdateModel = (index: number, field: keyof CustomModel, value: string | number) => {
    const updated = [...customModels];
    updated[index] = { ...updated[index], [field]: value };
    setCustomModels(updated);
  };

  const handleSubmitCustomProvider = async () => {
    if (!customName.trim() || !customBaseUrl.trim()) {
      toast.error('Name and Base URL are required');
      return;
    }

    // Filter out empty models
    const validModels = customModels.filter(m => m.id.trim());
    if (validModels.length === 0) {
      toast.error('At least one model is required');
      return;
    }

    setLoading(true);
    try {
      await createAIProvider({
        provider_type: 'custom',
        name: customName,
        base_url: customBaseUrl,
        api_key: customApiKey || undefined,
        custom_env_var: customEnvVar || undefined,
        custom_models: validModels.map(m => ({
          id: m.id,
          name: m.name || undefined,
          context_limit: m.context_limit || undefined,
          output_limit: m.output_limit || undefined,
        })),
      });
      toast.success('Custom provider added');
      onSuccess();
      onClose();
    } catch (err) {
      toast.error(`Failed: ${err instanceof Error ? err.message : 'Unknown error'}`);
    } finally {
      setLoading(false);
    }
  };

  const handleBack = () => {
    if (step === 'select-method') {
      setStep('select-provider');
      setSelectedProvider(null);
    } else if (step === 'select-backends') {
      setStep('select-method');
    } else if (step === 'enter-api-key') {
      if (selectedProvider === 'anthropic' || selectedProvider === 'openai') {
        setStep('select-backends');
      } else if (hasOAuth) {
        setStep('select-method');
      } else {
        setStep('select-provider');
        setSelectedProvider(null);
      }
      setApiKey('');
      setAccountLabel('');
    } else if (step === 'custom-provider') {
      setStep('select-provider');
      setSelectedProvider(null);
      setCustomName('');
      setCustomBaseUrl('');
      setCustomApiKey('');
      setCustomEnvVar('');
      setCustomModels([{ id: '', name: '' }]);
    }
  };

  if (!open) return null;

  const getTitle = () => {
    switch (step) {
      case 'select-provider': return 'Add Provider';
      case 'select-method': return `Connect ${selectedTypeInfo?.name}`;
      case 'select-backends': return 'Select Backends';
      case 'enter-api-key': return `${selectedTypeInfo?.name} API Key`;
      case 'oauth-callback': return 'Complete Authorization';
      case 'custom-provider': return 'Custom Provider';
      default: return 'Add Provider';
    }
  };

  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center">
      {/* Backdrop */}
      <div className="absolute inset-0 bg-black/60 backdrop-blur-sm" />

      {/* Dialog */}
      <div
        ref={dialogRef}
        className={cn(
          "relative w-full max-h-[calc(100vh-2rem)] flex flex-col rounded-2xl bg-[#1a1a1a] border border-white/[0.06] shadow-xl animate-in fade-in zoom-in-95 duration-200",
          step === 'custom-provider' ? 'max-w-md' : 'max-w-sm'
        )}
      >
        {/* Header */}
        <div className="flex items-center justify-between px-5 py-4 border-b border-white/[0.06]">
          <div className="flex items-center gap-3">
            {step !== 'select-provider' && step !== 'oauth-callback' && (
              <button
                onClick={handleBack}
                disabled={loading}
                className="p-1 -ml-1 rounded-lg text-white/40 hover:text-white/70 hover:bg-white/[0.08] transition-colors cursor-pointer disabled:opacity-50"
              >
                <svg className="h-4 w-4" fill="none" viewBox="0 0 24 24" stroke="currentColor">
                  <path strokeLinecap="round" strokeLinejoin="round" strokeWidth={2} d="M15 19l-7-7 7-7" />
                </svg>
              </button>
            )}
            <h3 className="text-base font-semibold text-white">{getTitle()}</h3>
          </div>
          <button
            onClick={handleClose}
            disabled={loading}
            className="p-1 rounded-lg text-white/40 hover:text-white/70 hover:bg-white/[0.08] transition-colors cursor-pointer disabled:opacity-50"
          >
            <X className="h-4 w-4" />
          </button>
        </div>

        {/* Content */}
        <div className="p-4 overflow-y-auto">
          {/* Step 1: Select Provider */}
          {step === 'select-provider' && (
            <div className="space-y-1">
              {providerTypes.map((type) => (
                <button
                  key={type.id}
                  onClick={() => handleSelectProvider(type.id as AIProviderType)}
                  className="w-full flex items-center gap-3 p-3 rounded-xl hover:bg-white/[0.04] transition-colors cursor-pointer text-left"
                >
                  <span className="text-xl">{providerIcons[type.id] || '🔧'}</span>
                  <span className="text-sm text-white">{type.name}</span>
                </button>
              ))}
              {/* Custom Provider Option */}
              <button
                onClick={() => handleSelectProvider('custom')}
                className="w-full flex items-center gap-3 p-3 rounded-xl hover:bg-white/[0.04] transition-colors cursor-pointer text-left border-t border-white/[0.06] mt-2 pt-3"
              >
                <span className="text-xl">🔧</span>
                <div>
                  <span className="text-sm text-white">Custom Provider</span>
                  <div className="text-xs text-white/40">OpenAI-compatible endpoint</div>
                </div>
              </button>
            </div>
          )}

          {/* Step 2: Select Auth Method */}
          {step === 'select-method' && (
            <div className="space-y-1">
              {authMethods.map((method, index) => (
                <button
                  key={method.label}
                  onClick={() => handleSelectMethod(index)}
                  disabled={loading}
                  className={cn(
                    'w-full flex items-center gap-3 p-3 rounded-xl transition-colors cursor-pointer text-left',
                    'hover:bg-white/[0.04]',
                    loading && 'opacity-50 cursor-not-allowed'
                  )}
                >
                  <div className="flex h-8 w-8 items-center justify-center rounded-lg bg-white/[0.04]">
                    {loading && selectedMethodIndex === index ? (
                      <Loader className="h-4 w-4 text-white/40 animate-spin" />
                    ) : method.type === 'api' ? (
                      <Key className="h-4 w-4 text-white/40" />
                    ) : (
                      <ExternalLink className="h-4 w-4 text-white/40" />
                    )}
                  </div>
                  <div className="flex-1">
                    <div className="text-sm text-white">{method.label}</div>
                    {method.description && (
                      <div className="text-xs text-white/40">{method.description}</div>
                    )}
                  </div>
                </button>
              ))}
            </div>
          )}

          {/* Step 2.5: Select Backends */}
          {step === 'select-backends' && (
            <div className="space-y-4">
              <p className="text-sm text-white/60">
                Choose which backends should use this {selectedTypeInfo?.name} provider:
              </p>
              {selectedProvider === 'openai' &&
                (() => {
                  const method =
                    selectedMethodIndex !== null ? authMethods[selectedMethodIndex] : null;
                  const isOAuth = method?.type === 'oauth';
                  if (!isOAuth) return null;
                  return (
                    <div className="rounded-xl border border-amber-500/20 bg-amber-500/10 px-3 py-2 text-xs text-amber-200/80">
                      If you enable Codex, we will mint an OpenAI API key during connect (this is what the Codex CLI does after OAuth).
                    </div>
                  );
                })()}
              <div className="space-y-2">
                {selectedProvider === 'anthropic' && (
                  <>
                    <label className="flex items-center gap-3 p-3 rounded-xl border border-white/[0.06] hover:bg-white/[0.02] transition-colors cursor-pointer">
                  <input
                    type="checkbox"
                    checked={selectedBackends.includes('opencode')}
                    onChange={() => toggleBackend('opencode')}
                    className="rounded border-white/20 bg-white/[0.02] text-indigo-500 focus:ring-indigo-500/30 cursor-pointer"
                  />
                  <div className="flex-1">
                    <div className="text-sm text-white">OpenCode</div>
                    <div className="text-xs text-white/40">Use for OpenCode agents and missions</div>
                  </div>
                </label>
                <label className="flex items-center gap-3 p-3 rounded-xl border border-white/[0.06] hover:bg-white/[0.02] transition-colors cursor-pointer">
                  <input
                    type="checkbox"
                    checked={selectedBackends.includes('claudecode')}
                    onChange={() => toggleBackend('claudecode')}
                    className="rounded border-white/20 bg-white/[0.02] text-indigo-500 focus:ring-indigo-500/30 cursor-pointer"
                  />
                  <div className="flex-1">
                    <div className="text-sm text-white">Claude Code</div>
                    <div className="text-xs text-white/40">Use for Claude CLI-based missions</div>
                  </div>
                </label>
                  </>
                )}

                {selectedProvider === 'openai' && (
                  <>
                    <label className="flex items-center gap-3 p-3 rounded-xl border border-white/[0.06] hover:bg-white/[0.02] transition-colors cursor-pointer">
                      <input
                        type="checkbox"
                        checked={selectedBackends.includes('opencode')}
                        onChange={() => toggleBackend('opencode')}
                        className="rounded border-white/20 bg-white/[0.02] text-indigo-500 focus:ring-indigo-500/30 cursor-pointer"
                      />
                      <div className="flex-1">
                        <div className="text-sm text-white">OpenCode</div>
                        <div className="text-xs text-white/40">Use for OpenCode agents and missions</div>
                      </div>
                    </label>

                    {(() => {
                      const method = selectedMethodIndex !== null ? authMethods[selectedMethodIndex] : null;
                      const disableCodex = false;
                      return (
                        <label
                          className={cn(
                            "flex items-center gap-3 p-3 rounded-xl border border-white/[0.06] transition-colors",
                            disableCodex ? "opacity-50 cursor-not-allowed" : "hover:bg-white/[0.02] cursor-pointer"
                          )}
                          title={disableCodex ? "Codex is disabled." : undefined}
                        >
                          <input
                            type="checkbox"
                            checked={selectedBackends.includes('codex')}
                            onChange={() => toggleBackend('codex')}
                            disabled={disableCodex}
                            className="rounded border-white/20 bg-white/[0.02] text-indigo-500 focus:ring-indigo-500/30 cursor-pointer"
                          />
                          <div className="flex-1">
                            <div className="text-sm text-white">Codex</div>
                            <div className="text-xs text-white/40">
                              Use for Codex CLI missions
                            </div>
                          </div>
                        </label>
                      );
                    })()}
                  </>
                )}
              </div>
              <button
                onClick={handleContinueFromBackends}
                disabled={loading || selectedBackends.length === 0}
                className="w-full rounded-xl bg-indigo-500 px-4 py-3 text-sm font-medium text-white hover:bg-indigo-600 transition-colors cursor-pointer disabled:opacity-50 disabled:cursor-not-allowed"
              >
                {loading ? <Loader className="h-4 w-4 animate-spin mx-auto" /> : 'Continue'}
              </button>
            </div>
          )}

          {/* Step 3: Enter API Key */}
          {step === 'enter-api-key' && (
            <div className="space-y-4">
              <div>
                <label className="block text-xs text-white/50 mb-1.5">Account label (optional)</label>
                <input
                  type="text"
                  value={accountLabel}
                  onChange={(e) => setAccountLabel(e.target.value)}
                  placeholder="e.g. Team, Personal, Ben"
                  className="w-full rounded-xl border border-white/[0.06] bg-white/[0.02] px-4 py-2.5 text-sm text-white placeholder-white/30 focus:outline-none focus:border-indigo-500/50"
                />
                <p className="text-xs text-white/30 mt-1">Distinguish multiple accounts of the same provider</p>
              </div>
              <input
                type="password"
                value={apiKey}
                onChange={(e) => setApiKey(e.target.value)}
                placeholder="sk-..."
                autoFocus
                className="w-full rounded-xl border border-white/[0.06] bg-white/[0.02] px-4 py-3 text-sm text-white placeholder-white/30 focus:outline-none focus:border-indigo-500/50"
              />
              <button
                onClick={handleSubmitApiKey}
                disabled={loading || !apiKey.trim()}
                className="w-full rounded-xl bg-indigo-500 px-4 py-3 text-sm font-medium text-white hover:bg-indigo-600 transition-colors cursor-pointer disabled:opacity-50 disabled:cursor-not-allowed"
              >
                {loading ? <Loader className="h-4 w-4 animate-spin mx-auto" /> : 'Add Provider'}
              </button>
            </div>
          )}

          {/* Step 4: OAuth Callback */}
          {step === 'oauth-callback' && oauthResponse && (
            <div className="space-y-4">
              <div className="text-sm text-white/60 whitespace-pre-line">
                {oauthResponse.instructions}
              </div>
              <input
                type="text"
                value={oauthCode}
                onChange={(e) => setOauthCode(e.target.value)}
                placeholder="sk-ant-oc01-...#..."
                autoFocus
                className="w-full rounded-xl border border-white/[0.06] bg-white/[0.02] px-4 py-3 text-sm text-white placeholder-white/30 focus:outline-none focus:border-indigo-500/50 font-mono"
              />
              <div className="flex gap-2">
                <button
                  onClick={() => window.open(oauthResponse.url, '_blank')}
                  className="flex-1 rounded-xl border border-white/[0.06] px-4 py-3 text-sm text-white/70 hover:bg-white/[0.04] transition-colors cursor-pointer"
                >
                  Open Link Again
                </button>
                <button
                  onClick={handleSubmitOAuthCode}
                  disabled={loading || !oauthCode.trim()}
                  className="flex-1 rounded-xl bg-indigo-500 px-4 py-3 text-sm font-medium text-white hover:bg-indigo-600 transition-colors cursor-pointer disabled:opacity-50 disabled:cursor-not-allowed"
                >
                  {loading ? <Loader className="h-4 w-4 animate-spin mx-auto" /> : 'Connect'}
                </button>
              </div>
            </div>
          )}

          {/* Step 5: Custom Provider Form */}
          {step === 'custom-provider' && (
            <div className="space-y-4">
              {/* Name */}
              <div>
                <label className="block text-xs text-white/50 mb-1.5">Provider Name *</label>
                <input
                  type="text"
                  value={customName}
                  onChange={(e) => setCustomName(e.target.value)}
                  placeholder="My Self-Hosted Router"
                  autoFocus
                  className="w-full rounded-xl border border-white/[0.06] bg-white/[0.02] px-4 py-2.5 text-sm text-white placeholder-white/30 focus:outline-none focus:border-indigo-500/50"
                />
              </div>

              {/* Base URL */}
              <div>
                <label className="block text-xs text-white/50 mb-1.5">Base URL *</label>
                <input
                  type="url"
                  value={customBaseUrl}
                  onChange={(e) => setCustomBaseUrl(e.target.value)}
                  placeholder="https://api.example.com/v1"
                  className="w-full rounded-xl border border-white/[0.06] bg-white/[0.02] px-4 py-2.5 text-sm text-white placeholder-white/30 focus:outline-none focus:border-indigo-500/50 font-mono"
                />
              </div>

              {/* API Key (optional) */}
              <div>
                <label className="block text-xs text-white/50 mb-1.5">API Key (optional)</label>
                <input
                  type="password"
                  value={customApiKey}
                  onChange={(e) => setCustomApiKey(e.target.value)}
                  placeholder="sk-..."
                  className="w-full rounded-xl border border-white/[0.06] bg-white/[0.02] px-4 py-2.5 text-sm text-white placeholder-white/30 focus:outline-none focus:border-indigo-500/50"
                />
              </div>

              {/* Env Var Name (optional) */}
              <div>
                <label className="block text-xs text-white/50 mb-1.5">Environment Variable (optional)</label>
                <input
                  type="text"
                  value={customEnvVar}
                  onChange={(e) => setCustomEnvVar(e.target.value)}
                  placeholder="MY_CUSTOM_API_KEY"
                  className="w-full rounded-xl border border-white/[0.06] bg-white/[0.02] px-4 py-2.5 text-sm text-white placeholder-white/30 focus:outline-none focus:border-indigo-500/50 font-mono"
                />
                <p className="text-xs text-white/30 mt-1">If set, OpenCode will use this env var for the API key</p>
              </div>

              {/* Models */}
              <div>
                <div className="flex items-center justify-between mb-1.5">
                  <label className="text-xs text-white/50">Models *</label>
                  <button
                    type="button"
                    onClick={handleAddModel}
                    className="flex items-center gap-1 text-xs text-indigo-400 hover:text-indigo-300 transition-colors cursor-pointer"
                  >
                    <Plus className="h-3 w-3" />
                    Add Model
                  </button>
                </div>
                <div className="space-y-2 max-h-48 overflow-y-auto">
                  {customModels.map((model, index) => (
                    <div key={index} className="flex gap-2 items-start">
                      <div className="flex-1 space-y-1">
                        <input
                          type="text"
                          value={model.id}
                          onChange={(e) => handleUpdateModel(index, 'id', e.target.value)}
                          placeholder="model-id (required)"
                          className="w-full rounded-lg border border-white/[0.06] bg-white/[0.02] px-3 py-2 text-xs text-white placeholder-white/30 focus:outline-none focus:border-indigo-500/50 font-mono"
                        />
                        <input
                          type="text"
                          value={model.name || ''}
                          onChange={(e) => handleUpdateModel(index, 'name', e.target.value)}
                          placeholder="Display name (optional)"
                          className="w-full rounded-lg border border-white/[0.06] bg-white/[0.02] px-3 py-2 text-xs text-white placeholder-white/30 focus:outline-none focus:border-indigo-500/50"
                        />
                      </div>
                      {customModels.length > 1 && (
                        <button
                          type="button"
                          onClick={() => handleRemoveModel(index)}
                          className="p-2 text-white/30 hover:text-red-400 transition-colors cursor-pointer"
                        >
                          <Trash2 className="h-3.5 w-3.5" />
                        </button>
                      )}
                    </div>
                  ))}
                </div>
              </div>

              <button
                onClick={handleSubmitCustomProvider}
                disabled={loading || !customName.trim() || !customBaseUrl.trim() || !customModels.some(m => m.id.trim())}
                className="w-full rounded-xl bg-indigo-500 px-4 py-3 text-sm font-medium text-white hover:bg-indigo-600 transition-colors cursor-pointer disabled:opacity-50 disabled:cursor-not-allowed"
              >
                {loading ? <Loader className="h-4 w-4 animate-spin mx-auto" /> : 'Add Custom Provider'}
              </button>
            </div>
          )}
        </div>
      </div>
    </div>
  );
}
