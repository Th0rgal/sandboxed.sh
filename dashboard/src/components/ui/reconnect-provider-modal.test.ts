import { describe, expect, it } from 'vitest';
import type { AIProvider } from '@/lib/api';
import {
  providerShouldShowReconnect,
  providerSupportsOAuthReconnect,
} from './reconnect-provider-modal';

function xaiProvider(status: AIProvider['status']['type']): AIProvider {
  return {
    id: 'xai-account',
    provider_type: 'xai',
    provider_type_name: 'xAI',
    name: 'xAI',
    has_api_key: false,
    has_oauth: true,
    base_url: null,
    enabled: true,
    is_default: false,
    uses_oauth: true,
    auth_methods: [],
    status: { type: status },
    use_for_backends: ['opencode', 'grok'],
    created_at: '2026-01-01T00:00:00Z',
    updated_at: '2026-01-01T00:00:00Z',
  };
}

describe('OAuth reconnect availability', () => {
  it('keeps reconnect available while a provider is reported connected', () => {
    const provider = xaiProvider('connected');

    expect(providerSupportsOAuthReconnect(provider)).toBe(true);
    expect(providerShouldShowReconnect(provider, 'connected')).toBe(true);
  });

  it('keeps reconnect available when xAI requires reauthorization', () => {
    expect(providerShouldShowReconnect(xaiProvider('needs_reauth'), 'needs_reauth')).toBe(true);
  });
});
