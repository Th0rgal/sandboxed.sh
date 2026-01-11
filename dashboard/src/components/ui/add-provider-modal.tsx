'use client';

import { useState, useRef, useEffect } from 'react';
import { X, ExternalLink, Key, Loader, Cpu } from 'lucide-react';
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
} from '@/lib/api';

// Provider icons mapping
const providerIcons: Record<string, string> = {
  anthropic: '🧠',
  openai: '🤖',
  google: '🔮',
  'amazon-bedrock': '☁️',
  azure: '⚡',
  'open-router': '🔀',
  mistral: '🌪️',
  groq: '⚡',
  xai: '𝕏',
  'github-copilot': '🐙',
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
        label: 'ChatGPT Plus/Pro (Codex Subscription)',
        type: 'oauth',
        description: 'Use your ChatGPT Plus/Pro subscription via official OAuth',
      },
      {
        label: 'ChatGPT Plus/Pro (Manual URL Paste)',
        type: 'oauth',
        description: 'Paste the full redirect URL if the callback fails',
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
  if (providerType === 'github-copilot') {
    return [
      { label: 'GitHub Copilot', type: 'oauth', description: 'Connect your subscription' },
    ];
  }
  return [];
};

type ModalStep = 'select-provider' | 'select-method' | 'enter-api-key' | 'oauth-callback';

export function AddProviderModal({ open, onClose, onSuccess, providerTypes }: AddProviderModalProps) {
  const dialogRef = useRef<HTMLDivElement>(null);

  // State
  const [step, setStep] = useState<ModalStep>('select-provider');
  const [selectedProvider, setSelectedProvider] = useState<AIProviderType | null>(null);
  const [selectedMethodIndex, setSelectedMethodIndex] = useState<number | null>(null);
  const [apiKey, setApiKey] = useState('');
  const [oauthResponse, setOauthResponse] = useState<OAuthAuthorizeResponse | null>(null);
  const [oauthCode, setOauthCode] = useState('');
  const [loading, setLoading] = useState(false);

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
      setOauthResponse(null);
      setOauthCode('');
      setLoading(false);
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
    const typeInfo = providerTypes.find(t => t.id === providerType);
    const methods = getProviderAuthMethods(providerType);

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

  const handleSubmitApiKey = async () => {
    if (!apiKey.trim() || !selectedProvider) return;

    setLoading(true);
    try {
      await createAIProvider({
        provider_type: selectedProvider,
        name: selectedTypeInfo?.name || selectedProvider,
        api_key: apiKey,
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
      await oauthCallback(selectedProvider, selectedMethodIndex, oauthCode);
      toast.success('Provider connected');
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
    } else if (step === 'enter-api-key') {
      if (hasOAuth) {
        setStep('select-method');
      } else {
        setStep('select-provider');
        setSelectedProvider(null);
      }
      setApiKey('');
    }
  };

  if (!open) return null;

  const getTitle = () => {
    switch (step) {
      case 'select-provider': return 'Add Provider';
      case 'select-method': return `Connect ${selectedTypeInfo?.name}`;
      case 'enter-api-key': return `${selectedTypeInfo?.name} API Key`;
      case 'oauth-callback': return 'Complete Authorization';
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
        className="relative w-full max-w-sm rounded-2xl bg-[#1a1a1a] border border-white/[0.06] shadow-xl animate-in fade-in zoom-in-95 duration-200"
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
        <div className="p-4">
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

          {/* Step 3: Enter API Key */}
          {step === 'enter-api-key' && (
            <div className="space-y-4">
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
              <p className="text-sm text-white/60">
                {oauthResponse.instructions}
              </p>
              <input
                type="text"
                value={oauthCode}
                onChange={(e) => setOauthCode(e.target.value)}
                placeholder="Authorization code or redirect URL"
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
        </div>
      </div>
    </div>
  );
}
