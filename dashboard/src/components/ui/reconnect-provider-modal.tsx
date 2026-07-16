'use client';

import { useState, useRef, useEffect, useCallback } from 'react';
import { X, ExternalLink, Loader } from 'lucide-react';
import { toast } from '@/components/toast';
import { cn } from '@/lib/utils';
import {
  oauthAuthorize,
  oauthCallback,
  AIProvider,
  OAuthAuthorizeResponse,
} from '@/lib/api';

interface ReconnectMethod {
  /** Index into the backend `ProviderType::auth_methods()` list. */
  index: number;
  label: string;
  description: string;
}

// OAuth methods available for reconnecting each provider type. The `index`
// MUST match the ordering of `ProviderType::auth_methods()` in
// `src/api/ai_providers.rs`, since the backend resolves the method (and, for
// Anthropic, the OAuth mode) by that index.
const RECONNECT_OAUTH_METHODS: Record<string, ReconnectMethod[]> = {
  anthropic: [
    { index: 0, label: 'Claude Pro/Max', description: 'Reconnect with your Claude subscription' },
    { index: 1, label: 'Create API Key', description: 'Create a new key via OAuth' },
  ],
  openai: [
    { index: 0, label: 'ChatGPT Plus/Pro (OAuth)', description: 'Reconnect via official OpenAI OAuth' },
  ],
  google: [
    { index: 0, label: 'OAuth with Google (Gemini CLI)', description: 'Reconnect via Google OAuth' },
  ],
  xai: [
    { index: 0, label: 'Grok Build OAuth', description: 'Reconnect via grok.com device authorization' },
  ],
};

export function providerSupportsOAuthReconnect(provider: AIProvider): boolean {
  return (
    provider.uses_oauth &&
    !provider.has_api_key &&
    RECONNECT_OAUTH_METHODS[provider.provider_type] !== undefined
  );
}

export function providerShouldShowReconnect(
  provider: AIProvider,
  effectiveStatus: AIProvider['status']['type']
): boolean {
  return (
    providerSupportsOAuthReconnect(provider) ||
    effectiveStatus === 'needs_auth' ||
    effectiveStatus === 'needs_reauth' ||
    effectiveStatus === 'error'
  );
}

interface ReconnectProviderModalProps {
  provider: AIProvider | null;
  open: boolean;
  onClose: () => void;
  onSuccess: (providerId: string) => void;
}

type Step = 'select-method' | 'oauth-callback';

export function ReconnectProviderModal({
  provider,
  open,
  onClose,
  onSuccess,
}: ReconnectProviderModalProps) {
  const dialogRef = useRef<HTMLDivElement>(null);
  // Monotonic token to discard stale `oauthAuthorize` responses: closing the
  // modal or switching providers while a request is in flight bumps this, so a
  // late response can't apply its URL/step/loading state to a different (or
  // already-closed) provider.
  const authorizeReqRef = useRef(0);

  const [step, setStep] = useState<Step>('select-method');
  const [methodIndex, setMethodIndex] = useState<number | null>(null);
  const [oauthResponse, setOauthResponse] = useState<OAuthAuthorizeResponse | null>(null);
  const [oauthCode, setOauthCode] = useState('');
  const [loading, setLoading] = useState(false);

  const methods = provider ? RECONNECT_OAUTH_METHODS[provider.provider_type] ?? [] : [];

  const startAuthorize = useCallback(
    async (index: number) => {
      if (!provider) return;
      const reqId = ++authorizeReqRef.current;
      setMethodIndex(index);
      setLoading(true);
      try {
        const response = await oauthAuthorize(provider.id, index);
        // A newer authorize started, or the modal closed/switched providers —
        // drop this stale response so it can't hijack the current state.
        if (authorizeReqRef.current !== reqId) return;
        setOauthResponse(response);
        setStep('oauth-callback');
        window.open(response.url, '_blank');
      } catch (err) {
        if (authorizeReqRef.current !== reqId) return;
        toast.error(`Failed: ${err instanceof Error ? err.message : 'Unknown error'}`);
      } finally {
        if (authorizeReqRef.current === reqId) setLoading(false);
      }
    },
    [provider]
  );

  // Reset on open. When only a single OAuth method exists (e.g. xAI), skip the
  // selection step and kick off authorization immediately.
  useEffect(() => {
    if (open && provider) {
      // Invalidate any authorize request still in flight from a previous open.
      authorizeReqRef.current += 1;
      setStep('select-method');
      setMethodIndex(null);
      setOauthResponse(null);
      setOauthCode('');
      setLoading(false);
      if (methods.length === 1) {
        startAuthorize(methods[0].index);
      }
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [open, provider?.id]);

  const handleClose = useCallback(() => {
    // Supersede any in-flight authorize so its late response is ignored.
    authorizeReqRef.current += 1;
    onClose();
  }, [onClose]);

  // Escape to close
  useEffect(() => {
    const handleKeyDown = (e: KeyboardEvent) => {
      if (e.key === 'Escape' && open && !loading) handleClose();
    };
    document.addEventListener('keydown', handleKeyDown);
    return () => document.removeEventListener('keydown', handleKeyDown);
  }, [handleClose, open, loading]);

  // Click outside to close
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
  }, [handleClose, open, loading]);

  const handleSubmitCode = async () => {
    if (!provider || methodIndex === null) return;
    if (!oauthCode.trim() && oauthResponse?.method !== 'auto') return;

    setLoading(true);
    try {
      await oauthCallback(provider.id, methodIndex, oauthCode, provider.use_for_backends);
      // Don't celebrate yet — `onSuccess` runs a usage probe and owns the
      // success/failure toast, so we avoid showing "reconnected" followed by a
      // "check still fails" error for the same action.
      onSuccess(provider.id);
      onClose();
    } catch (err) {
      toast.error(`Failed: ${err instanceof Error ? err.message : 'Unknown error'}`);
    } finally {
      setLoading(false);
    }
  };

  if (!open || !provider) return null;

  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center">
      <div className="absolute inset-0 bg-black/60 backdrop-blur-sm" />

      <div
        ref={dialogRef}
        className="relative w-full max-w-sm max-h-[calc(100vh-2rem)] flex flex-col rounded-2xl bg-[#1a1a1a] border border-white/[0.06] shadow-xl animate-in fade-in zoom-in-95 duration-200"
      >
        {/* Header */}
        <div className="flex items-center justify-between px-5 py-4 border-b border-white/[0.06]">
          <h3 className="text-base font-semibold text-white">
            Reconnect {provider.label || provider.provider_type_name}
          </h3>
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
          {step === 'select-method' && (
            <div className="space-y-1">
              {methods.length === 0 ? (
                <p className="text-sm text-white/60 p-3">
                  No OAuth reconnect method available for this provider.
                </p>
              ) : (
                methods.map((method) => (
                  <button
                    key={method.index}
                    onClick={() => startAuthorize(method.index)}
                    disabled={loading}
                    className={cn(
                      'w-full flex items-center gap-3 p-3 rounded-xl transition-colors cursor-pointer text-left hover:bg-white/[0.04]',
                      loading && 'opacity-50 cursor-not-allowed'
                    )}
                  >
                    <div className="flex h-8 w-8 items-center justify-center rounded-lg bg-white/[0.04]">
                      {loading && methodIndex === method.index ? (
                        <Loader className="h-4 w-4 text-white/40 animate-spin" />
                      ) : (
                        <ExternalLink className="h-4 w-4 text-white/40" />
                      )}
                    </div>
                    <div className="flex-1">
                      <div className="text-sm text-white">{method.label}</div>
                      <div className="text-xs text-white/40">{method.description}</div>
                    </div>
                  </button>
                ))
              )}
            </div>
          )}

          {step === 'oauth-callback' && oauthResponse && (
            <div className="space-y-4">
              <div className="text-sm text-white/60 whitespace-pre-line">
                {oauthResponse.instructions}
              </div>
              <input
                type="text"
                value={oauthCode}
                onChange={(e) => setOauthCode(e.target.value)}
                placeholder={oauthResponse.method === 'auto' ? 'No code required' : 'sk-ant-oc01-...#...'}
                disabled={oauthResponse.method === 'auto'}
                autoFocus
                className="w-full rounded-xl border border-white/[0.06] bg-white/[0.02] px-4 py-3 text-sm text-white placeholder-white/30 focus:outline-none focus:border-indigo-500/50 font-mono disabled:opacity-60"
              />
              <div className="flex gap-2">
                <button
                  onClick={() => window.open(oauthResponse.url, '_blank')}
                  className="flex-1 rounded-xl border border-white/[0.06] px-4 py-3 text-sm text-white/70 hover:bg-white/[0.04] transition-colors cursor-pointer"
                >
                  Open Link Again
                </button>
                <button
                  onClick={handleSubmitCode}
                  disabled={loading || (!oauthCode.trim() && oauthResponse.method !== 'auto')}
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
