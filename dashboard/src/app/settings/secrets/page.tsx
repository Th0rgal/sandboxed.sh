'use client';

import { useState, useEffect } from 'react';
import {
  getSecretsStatus,
  initializeSecrets,
  unlockSecrets,
  lockSecrets,
  listSecrets,
  setSecret,
  deleteSecret,
  revealSecret,
  type SecretsStatus,
  type SecretInfo,
  type RegistryInfo,
} from '@/lib/api';
import {
  Key,
  Lock,
  Unlock,
  Plus,
  Trash2,
  Eye,
  EyeOff,
  Loader,
  Shield,
  Copy,
  Check,
  X,
} from 'lucide-react';
import { cn } from '@/lib/utils';
import { useToast } from '@/components/toast';

export default function SecretsPage() {
  const [status, setStatus] = useState<SecretsStatus | null>(null);
  const [loading, setLoading] = useState(true);
  const { showError } = useToast();

  // Unlock dialog
  const [showUnlockDialog, setShowUnlockDialog] = useState(false);
  const [passphrase, setPassphrase] = useState('');
  const [unlocking, setUnlocking] = useState(false);

  // Initialize dialog
  const [showInitDialog, setShowInitDialog] = useState(false);
  const [initializing, setInitializing] = useState(false);

  // Selected registry and secrets
  const [selectedRegistry, setSelectedRegistry] = useState<string | null>(null);
  const [secrets, setSecrets] = useState<SecretInfo[]>([]);
  const [loadingSecrets, setLoadingSecrets] = useState(false);

  // Add secret dialog
  const [showAddDialog, setShowAddDialog] = useState(false);
  const [newSecretRegistry, setNewSecretRegistry] = useState('');
  const [newSecretKey, setNewSecretKey] = useState('');
  const [newSecretValue, setNewSecretValue] = useState('');
  const [newSecretType, setNewSecretType] = useState<string>('generic');
  const [addingSecret, setAddingSecret] = useState(false);

  // Reveal secret
  const [revealedSecrets, setRevealedSecrets] = useState<Record<string, string>>({});
  const [revealingSecret, setRevealingSecret] = useState<string | null>(null);

  // Copy feedback
  const [copiedKey, setCopiedKey] = useState<string | null>(null);

  // Load status on mount
  useEffect(() => {
    loadStatus();
  }, []);

  // Load secrets when registry changes
  useEffect(() => {
    if (selectedRegistry && status?.can_decrypt) {
      loadSecrets(selectedRegistry);
    }
  }, [selectedRegistry, status?.can_decrypt]);

  // Handle ESC key to close modals
  useEffect(() => {
    const handleKeyDown = (e: KeyboardEvent) => {
      if (e.key === 'Escape') {
        if (showInitDialog) setShowInitDialog(false);
        if (showUnlockDialog) setShowUnlockDialog(false);
        if (showAddDialog) setShowAddDialog(false);
      }
    };
    document.addEventListener('keydown', handleKeyDown);
    return () => document.removeEventListener('keydown', handleKeyDown);
  }, [showInitDialog, showUnlockDialog, showAddDialog]);

  const loadStatus = async () => {
    try {
      setLoading(true);
      const s = await getSecretsStatus();
      setStatus(s);
      if (s.registries.length > 0 && !selectedRegistry) {
        setSelectedRegistry(s.registries[0].name);
      }
    } catch (err) {
      showError(err instanceof Error ? err.message : 'Failed to load secrets status');
    } finally {
      setLoading(false);
    }
  };

  const loadSecrets = async (registry: string) => {
    try {
      setLoadingSecrets(true);
      const s = await listSecrets(registry);
      setSecrets(s);
      setRevealedSecrets({});
    } catch (err) {
      console.error('Failed to load secrets:', err);
      setSecrets([]);
    } finally {
      setLoadingSecrets(false);
    }
  };

  const handleInitialize = async () => {
    try {
      setInitializing(true);
      const result = await initializeSecrets('default');
      setShowInitDialog(false);
      await loadStatus();
      // Show message about setting passphrase
      alert(result.message);
    } catch (err) {
      showError(err instanceof Error ? err.message : 'Failed to initialize');
    } finally {
      setInitializing(false);
    }
  };

  const handleUnlock = async () => {
    try {
      setUnlocking(true);
      await unlockSecrets(passphrase);
      setShowUnlockDialog(false);
      setPassphrase('');
      await loadStatus();
    } catch (err) {
      showError(err instanceof Error ? err.message : 'Invalid passphrase');
    } finally {
      setUnlocking(false);
    }
  };

  const handleLock = async () => {
    try {
      await lockSecrets();
      setRevealedSecrets({});
      await loadStatus();
    } catch (err) {
      showError(err instanceof Error ? err.message : 'Failed to lock');
    }
  };

  const handleAddSecret = async () => {
    if (!newSecretKey.trim() || !newSecretValue.trim() || !newSecretRegistry.trim()) return;
    try {
      setAddingSecret(true);
      await setSecret(newSecretRegistry, newSecretKey, newSecretValue, {
        type: newSecretType as 'api_key' | 'password' | 'generic',
      });
      setShowAddDialog(false);
      setNewSecretKey('');
      setNewSecretValue('');
      setNewSecretRegistry('');
      setNewSecretType('generic');
      await loadStatus();
      if (selectedRegistry === newSecretRegistry) {
        await loadSecrets(selectedRegistry);
      } else {
        setSelectedRegistry(newSecretRegistry);
      }
    } catch (err) {
      showError(err instanceof Error ? err.message : 'Failed to add secret');
    } finally {
      setAddingSecret(false);
    }
  };

  const handleDeleteSecret = async (registry: string, key: string) => {
    if (!confirm(`Delete secret "${key}"?`)) return;
    try {
      await deleteSecret(registry, key);
      await loadStatus();
      if (selectedRegistry === registry) {
        await loadSecrets(registry);
      }
    } catch (err) {
      showError(err instanceof Error ? err.message : 'Failed to delete secret');
    }
  };

  const handleReveal = async (registry: string, key: string) => {
    const fullKey = `${registry}/${key}`;
    if (revealedSecrets[fullKey]) {
      // Hide it
      setRevealedSecrets((prev) => {
        const next = { ...prev };
        delete next[fullKey];
        return next;
      });
      return;
    }

    try {
      setRevealingSecret(fullKey);
      const value = await revealSecret(registry, key);
      setRevealedSecrets((prev) => ({ ...prev, [fullKey]: value }));
    } catch (err) {
      showError(err instanceof Error ? err.message : 'Failed to reveal secret');
    } finally {
      setRevealingSecret(null);
    }
  };

  const handleCopy = async (registry: string, key: string) => {
    const fullKey = `${registry}/${key}`;
    try {
      let value = revealedSecrets[fullKey];
      if (!value) {
        value = await revealSecret(registry, key);
      }
      await navigator.clipboard.writeText(value);
      setCopiedKey(fullKey);
      setTimeout(() => setCopiedKey(null), 2000);
    } catch (err) {
      showError(err instanceof Error ? err.message : 'Failed to copy');
    }
  };

  const formatSecretType = (type: string | null) => {
    if (!type) return 'Generic';
    return type.replace(/_/g, ' ').replace(/\b\w/g, (c) => c.toUpperCase());
  };

  if (loading) {
    return (
      <div className="flex items-center justify-center min-h-[calc(100vh-4rem)]">
        <Loader className="h-8 w-8 animate-spin text-white/40" />
      </div>
    );
  }

  return (
    <div className="p-6 max-w-4xl mx-auto">
      <div className="mb-8">
        <div className="flex items-center justify-between">
          <div>
            <h1 className="text-2xl font-semibold text-white mb-2">Secrets</h1>
            <p className="text-white/50">
              Encrypted storage for OAuth tokens, API keys, and credentials.
            </p>
          </div>
          {status?.initialized && (
            <div className="flex items-center gap-2">
              {status.can_decrypt ? (
                <button
                  onClick={handleLock}
                  className="flex items-center gap-2 px-4 py-2 text-sm font-medium text-amber-400 bg-amber-500/10 hover:bg-amber-500/20 rounded-lg transition-colors"
                >
                  <Lock className="h-4 w-4" />
                  Lock
                </button>
              ) : (
                <button
                  onClick={() => setShowUnlockDialog(true)}
                  className="flex items-center gap-2 px-4 py-2 text-sm font-medium text-emerald-400 bg-emerald-500/10 hover:bg-emerald-500/20 rounded-lg transition-colors"
                >
                  <Unlock className="h-4 w-4" />
                  Unlock
                </button>
              )}
              <button
                onClick={() => {
                  setNewSecretRegistry(selectedRegistry || 'mcp-tokens');
                  setShowAddDialog(true);
                }}
                disabled={!status.can_decrypt}
                className="flex items-center gap-2 px-4 py-2 text-sm font-medium text-white bg-indigo-500 hover:bg-indigo-600 rounded-lg transition-colors disabled:opacity-50 disabled:cursor-not-allowed"
              >
                <Plus className="h-4 w-4" />
                Add Secret
              </button>
            </div>
          )}
        </div>
      </div>

      {!status?.initialized ? (
        // Not initialized - show setup
        <div className="rounded-xl bg-white/[0.02] border border-white/[0.06] p-8 text-center">
          <div className="w-16 h-16 rounded-full bg-indigo-500/10 flex items-center justify-center mx-auto mb-4">
            <Shield className="h-8 w-8 text-indigo-400" />
          </div>
          <h2 className="text-xl font-semibold text-white mb-2">Initialize Secrets</h2>
          <p className="text-white/50 mb-6 max-w-md mx-auto">
            Set up encrypted storage for sensitive data like OAuth tokens and API keys.
            You'll need to set a passphrase via environment variable.
          </p>
          <button
            onClick={() => setShowInitDialog(true)}
            className="px-6 py-3 text-sm font-medium text-white bg-indigo-500 hover:bg-indigo-600 rounded-lg transition-colors"
          >
            Initialize Secrets System
          </button>
        </div>
      ) : !status.can_decrypt ? (
        // Locked - show unlock prompt
        <div className="rounded-xl bg-white/[0.02] border border-white/[0.06] p-8 text-center">
          <div className="w-16 h-16 rounded-full bg-amber-500/10 flex items-center justify-center mx-auto mb-4">
            <Lock className="h-8 w-8 text-amber-400" />
          </div>
          <h2 className="text-xl font-semibold text-white mb-2">Secrets Locked</h2>
          <p className="text-white/50 mb-6 max-w-md mx-auto">
            Enter your passphrase to unlock and access your secrets.
            Or set OPENAGENT_SECRET_PASSPHRASE environment variable.
          </p>
          <button
            onClick={() => setShowUnlockDialog(true)}
            className="px-6 py-3 text-sm font-medium text-white bg-emerald-500 hover:bg-emerald-600 rounded-lg transition-colors"
          >
            Unlock Secrets
          </button>
        </div>
      ) : (
        // Unlocked - show secrets
        <div className="flex gap-6">
          {/* Registries sidebar */}
          <div className="w-64 flex-shrink-0">
            <div className="rounded-xl bg-white/[0.02] border border-white/[0.06] overflow-hidden">
              <div className="p-3 border-b border-white/[0.06]">
                <h3 className="text-sm font-medium text-white/60">Registries</h3>
              </div>
              <div className="p-2">
                {status.registries.length === 0 ? (
                  <p className="text-xs text-white/40 text-center py-4">No registries yet</p>
                ) : (
                  status.registries.map((registry) => (
                    <button
                      key={registry.name}
                      onClick={() => setSelectedRegistry(registry.name)}
                      className={cn(
                        'w-full text-left p-3 rounded-lg transition-colors mb-1',
                        selectedRegistry === registry.name
                          ? 'bg-white/[0.08] text-white'
                          : 'text-white/60 hover:bg-white/[0.04] hover:text-white'
                      )}
                    >
                      <div className="flex items-center gap-2">
                        <Key className="h-4 w-4" />
                        <span className="text-sm font-medium truncate">{registry.name}</span>
                      </div>
                      <p className="text-xs text-white/40 mt-1">
                        {registry.secret_count} secret{registry.secret_count !== 1 ? 's' : ''}
                      </p>
                    </button>
                  ))
                )}
              </div>
            </div>
          </div>

          {/* Secrets list */}
          <div className="flex-1">
            <div className="rounded-xl bg-white/[0.02] border border-white/[0.06] overflow-hidden">
              <div className="p-4 border-b border-white/[0.06] flex items-center justify-between">
                <h3 className="text-sm font-medium text-white">
                  {selectedRegistry ? `Secrets in ${selectedRegistry}` : 'Select a registry'}
                </h3>
              </div>
              <div className="divide-y divide-white/[0.06]">
                {loadingSecrets ? (
                  <div className="p-8 flex items-center justify-center">
                    <Loader className="h-5 w-5 animate-spin text-white/40" />
                  </div>
                ) : secrets.length === 0 ? (
                  <div className="p-8 text-center text-white/40 text-sm">
                    {selectedRegistry ? 'No secrets in this registry' : 'Select a registry to view secrets'}
                  </div>
                ) : (
                  secrets.map((secret) => {
                    const fullKey = `${selectedRegistry}/${secret.key}`;
                    const isRevealed = !!revealedSecrets[fullKey];
                    const isRevealing = revealingSecret === fullKey;
                    const isCopied = copiedKey === fullKey;

                    return (
                      <div key={secret.key} className="p-4 flex items-center gap-4">
                        <div className="flex-1 min-w-0">
                          <p className="text-sm font-medium text-white truncate">{secret.key}</p>
                          <div className="flex items-center gap-2 mt-1">
                            <span
                              className={cn(
                                'text-xs px-2 py-0.5 rounded',
                                secret.secret_type === 'api_key'
                                  ? 'bg-blue-500/10 text-blue-400'
                                  : secret.secret_type === 'oauth_access_token'
                                    ? 'bg-green-500/10 text-green-400'
                                    : secret.secret_type === 'password'
                                      ? 'bg-red-500/10 text-red-400'
                                      : 'bg-white/[0.06] text-white/50'
                              )}
                            >
                              {formatSecretType(secret.secret_type)}
                            </span>
                            {secret.is_expired && (
                              <span className="text-xs px-2 py-0.5 rounded bg-red-500/10 text-red-400">
                                Expired
                              </span>
                            )}
                          </div>
                          {isRevealed && (
                            <div className="mt-2 p-2 rounded bg-black/40 font-mono text-xs text-white/80 break-all">
                              {revealedSecrets[fullKey]}
                            </div>
                          )}
                        </div>
                        <div className="flex items-center gap-1">
                          <button
                            onClick={() => handleReveal(selectedRegistry!, secret.key)}
                            disabled={isRevealing}
                            className="p-2 rounded-lg text-white/40 hover:text-white hover:bg-white/[0.06] transition-colors"
                            title={isRevealed ? 'Hide' : 'Reveal'}
                          >
                            {isRevealing ? (
                              <Loader className="h-4 w-4 animate-spin" />
                            ) : isRevealed ? (
                              <EyeOff className="h-4 w-4" />
                            ) : (
                              <Eye className="h-4 w-4" />
                            )}
                          </button>
                          <button
                            onClick={() => handleCopy(selectedRegistry!, secret.key)}
                            className="p-2 rounded-lg text-white/40 hover:text-white hover:bg-white/[0.06] transition-colors"
                            title="Copy"
                          >
                            {isCopied ? (
                              <Check className="h-4 w-4 text-emerald-400" />
                            ) : (
                              <Copy className="h-4 w-4" />
                            )}
                          </button>
                          <button
                            onClick={() => handleDeleteSecret(selectedRegistry!, secret.key)}
                            className="p-2 rounded-lg text-red-400/60 hover:text-red-400 hover:bg-red-500/10 transition-colors"
                            title="Delete"
                          >
                            <Trash2 className="h-4 w-4" />
                          </button>
                        </div>
                      </div>
                    );
                  })
                )}
              </div>
            </div>
          </div>
        </div>
      )}

      {/* Initialize Dialog */}
      {showInitDialog && (
        <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/50">
          <div className="w-full max-w-md p-6 rounded-xl bg-[#1a1a1c] border border-white/[0.06]">
            <h3 className="text-lg font-medium text-white mb-4">Initialize Secrets</h3>
            <p className="text-sm text-white/60 mb-4">
              This will create the secrets configuration. After initialization, you need to set the{' '}
              <code className="px-1 py-0.5 rounded bg-white/[0.06] text-amber-400">
                OPENAGENT_SECRET_PASSPHRASE
              </code>{' '}
              environment variable with your chosen passphrase.
            </p>
            <div className="flex justify-end gap-2">
              <button
                onClick={() => setShowInitDialog(false)}
                className="px-4 py-2 text-sm text-white/60 hover:text-white"
              >
                Cancel
              </button>
              <button
                onClick={handleInitialize}
                disabled={initializing}
                className="px-4 py-2 text-sm font-medium text-white bg-indigo-500 hover:bg-indigo-600 rounded-lg disabled:opacity-50"
              >
                {initializing ? 'Initializing...' : 'Initialize'}
              </button>
            </div>
          </div>
        </div>
      )}

      {/* Unlock Dialog */}
      {showUnlockDialog && (
        <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/50">
          <div className="w-full max-w-md p-6 rounded-xl bg-[#1a1a1c] border border-white/[0.06]">
            <h3 className="text-lg font-medium text-white mb-4">Unlock Secrets</h3>
            <input
              type="password"
              placeholder="Enter passphrase..."
              value={passphrase}
              onChange={(e) => setPassphrase(e.target.value)}
              onKeyDown={(e) => e.key === 'Enter' && handleUnlock()}
              className="w-full px-4 py-2 rounded-lg bg-white/[0.04] border border-white/[0.08] text-white placeholder:text-white/30 focus:outline-none focus:border-indigo-500/50 mb-4"
              autoFocus
            />
            <div className="flex justify-end gap-2">
              <button
                onClick={() => {
                  setShowUnlockDialog(false);
                  setPassphrase('');
                }}
                className="px-4 py-2 text-sm text-white/60 hover:text-white"
              >
                Cancel
              </button>
              <button
                onClick={handleUnlock}
                disabled={!passphrase.trim() || unlocking}
                className="px-4 py-2 text-sm font-medium text-white bg-emerald-500 hover:bg-emerald-600 rounded-lg disabled:opacity-50"
              >
                {unlocking ? 'Unlocking...' : 'Unlock'}
              </button>
            </div>
          </div>
        </div>
      )}

      {/* Add Secret Dialog */}
      {showAddDialog && (
        <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/50">
          <div className="w-full max-w-md p-6 rounded-xl bg-[#1a1a1c] border border-white/[0.06]">
            <h3 className="text-lg font-medium text-white mb-4">Add Secret</h3>
            <div className="space-y-4">
              <div>
                <label className="block text-sm text-white/60 mb-1">Registry</label>
                <input
                  type="text"
                  placeholder="e.g., mcp-tokens"
                  value={newSecretRegistry}
                  onChange={(e) => setNewSecretRegistry(e.target.value)}
                  className="w-full px-4 py-2 rounded-lg bg-white/[0.04] border border-white/[0.08] text-white placeholder:text-white/30 focus:outline-none focus:border-indigo-500/50"
                />
              </div>
              <div>
                <label className="block text-sm text-white/60 mb-1">Key</label>
                <input
                  type="text"
                  placeholder="e.g., supabase/access_token"
                  value={newSecretKey}
                  onChange={(e) => setNewSecretKey(e.target.value)}
                  className="w-full px-4 py-2 rounded-lg bg-white/[0.04] border border-white/[0.08] text-white placeholder:text-white/30 focus:outline-none focus:border-indigo-500/50"
                />
              </div>
              <div>
                <label className="block text-sm text-white/60 mb-1">Value</label>
                <textarea
                  placeholder="Secret value..."
                  value={newSecretValue}
                  onChange={(e) => setNewSecretValue(e.target.value)}
                  rows={3}
                  className="w-full px-4 py-2 rounded-lg bg-white/[0.04] border border-white/[0.08] text-white placeholder:text-white/30 focus:outline-none focus:border-indigo-500/50 resize-none font-mono text-sm"
                />
              </div>
              <div>
                <label className="block text-sm text-white/60 mb-1">Type</label>
                <select
                  value={newSecretType}
                  onChange={(e) => setNewSecretType(e.target.value)}
                  className="w-full px-4 py-2 rounded-lg bg-white/[0.04] border border-white/[0.08] text-white focus:outline-none focus:border-indigo-500/50"
                >
                  <option value="generic">Generic</option>
                  <option value="api_key">API Key</option>
                  <option value="oauth_access_token">OAuth Access Token</option>
                  <option value="oauth_refresh_token">OAuth Refresh Token</option>
                  <option value="password">Password</option>
                </select>
              </div>
            </div>
            <div className="flex justify-end gap-2 mt-6">
              <button
                onClick={() => {
                  setShowAddDialog(false);
                  setNewSecretKey('');
                  setNewSecretValue('');
                  setNewSecretType('generic');
                }}
                className="px-4 py-2 text-sm text-white/60 hover:text-white"
              >
                Cancel
              </button>
              <button
                onClick={handleAddSecret}
                disabled={!newSecretKey.trim() || !newSecretValue.trim() || !newSecretRegistry.trim() || addingSecret}
                className="px-4 py-2 text-sm font-medium text-white bg-indigo-500 hover:bg-indigo-600 rounded-lg disabled:opacity-50"
              >
                {addingSecret ? 'Adding...' : 'Add Secret'}
              </button>
            </div>
          </div>
        </div>
      )}
    </div>
  );
}
