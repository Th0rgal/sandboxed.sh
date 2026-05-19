'use client';

import { useState, useEffect } from 'react';
import useSWR from 'swr';
import {
  getSecretsStatus,
  getEncryptionStatus,
  getPrivateKey,
  setPrivateKey,
  initializeSecrets,
  unlockSecrets,
  lockSecrets,
  listSecrets,
  setSecret,
  deleteSecret,
  revealSecret,
  getAuthStatus,
  changePassword,
  type SecretInfo,
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
  FileKey,
  Server,
  CheckCircle,
  AlertCircle,
  Settings,
  Clock,
  AlertTriangle,
  Info,
} from 'lucide-react';
import { cn } from '@/lib/utils';
import { useToast } from '@/components/toast';
import { toast } from '@/components/toast';

export default function SecretsPage() {
  const { showError } = useToast();

  // Fetch auth status
  const { data: authStatus, isLoading: authLoading, mutate: mutateAuth } = useSWR(
    'auth-status',
    getAuthStatus,
    { revalidateOnFocus: false }
  );

  // Fetch encryption status (skill content encryption)
  const { data: encryptionStatus, isLoading: encryptionLoading } = useSWR(
    'encryption-status',
    getEncryptionStatus,
    { revalidateOnFocus: false }
  );

  // Fetch secrets store status
  const { data: secretsStatus, isLoading: secretsLoading, mutate: mutateSecrets } = useSWR(
    'secrets-status',
    getSecretsStatus,
    { revalidateOnFocus: false }
  );

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

  // Password form state
  const [currentPassword, setCurrentPassword] = useState('');
  const [newPassword, setNewPassword] = useState('');
  const [confirmPassword, setConfirmPassword] = useState('');
  const [savingPassword, setSavingPassword] = useState(false);

  const isMultiUser = authStatus?.auth_mode === 'multi_user';
  const isAuthDisabled = authStatus?.auth_mode === 'disabled';
  const hasExistingPassword = authStatus?.password_source !== 'none';
  const requireCurrentPassword = hasExistingPassword && !isAuthDisabled;

  const handleChangePassword = async (e: React.FormEvent) => {
    e.preventDefault();
    if (newPassword !== confirmPassword) {
      toast.error('Passwords do not match');
      return;
    }
    if (newPassword.length < 8) {
      toast.error('Password must be at least 8 characters');
      return;
    }
    setSavingPassword(true);
    try {
      await changePassword({
        current_password: requireCurrentPassword ? currentPassword : undefined,
        new_password: newPassword,
      });
      toast.success('Password updated successfully');
      setCurrentPassword('');
      setNewPassword('');
      setConfirmPassword('');
      mutateAuth();
    } catch (err) {
      toast.error(err instanceof Error ? err.message : 'Failed to change password');
    } finally {
      setSavingPassword(false);
    }
  };

  const passwordSourceLabel = {
    dashboard: 'Dashboard-managed',
    environment: 'Environment variable',
    none: 'Not configured',
  }[authStatus?.password_source ?? 'none'];

  const authModeLabel = {
    disabled: 'Disabled',
    single_tenant: 'Single Tenant',
    multi_user: 'Multi-User',
  }[authStatus?.auth_mode ?? 'disabled'];

  // Private key management
  const [showKeyDialog, setShowKeyDialog] = useState(false);
  const [currentKeyHex, setCurrentKeyHex] = useState<string | null>(null);
  const [newKeyHex, setNewKeyHex] = useState('');
  const [loadingKey, setLoadingKey] = useState(false);
  const [savingKey, setSavingKey] = useState(false);
  const [showCurrentKey, setShowCurrentKey] = useState(false);
  const [keyDialogMode, setKeyDialogMode] = useState<'view' | 'edit'>('view');
  const [keyCopied, setKeyCopied] = useState(false);

  // Load secrets when registry changes
  useEffect(() => {
    if (selectedRegistry && secretsStatus?.can_decrypt) {
      loadSecrets(selectedRegistry);
    }
  }, [selectedRegistry, secretsStatus?.can_decrypt]);

  // Auto-select first registry
  useEffect(() => {
    if (secretsStatus?.registries.length && !selectedRegistry) {
      setSelectedRegistry(secretsStatus.registries[0].name);
    }
  }, [secretsStatus?.registries, selectedRegistry]);

  // Handle ESC key to close modals
  useEffect(() => {
    const handleKeyDown = (e: KeyboardEvent) => {
      if (e.key === 'Escape') {
        if (showInitDialog) setShowInitDialog(false);
        if (showUnlockDialog) setShowUnlockDialog(false);
        if (showAddDialog) setShowAddDialog(false);
        if (showKeyDialog) setShowKeyDialog(false);
      }
    };
    document.addEventListener('keydown', handleKeyDown);
    return () => document.removeEventListener('keydown', handleKeyDown);
  }, [showInitDialog, showUnlockDialog, showAddDialog, showKeyDialog]);

  const handleOpenKeyDialog = async () => {
    setShowKeyDialog(true);
    setKeyDialogMode('view');
    setShowCurrentKey(false);
    setNewKeyHex('');
    setLoadingKey(true);
    try {
      const response = await getPrivateKey();
      setCurrentKeyHex(response.key_hex);
    } catch (err) {
      console.error('Failed to load private key:', err);
      setCurrentKeyHex(null);
    } finally {
      setLoadingKey(false);
    }
  };

  const handleSaveKey = async () => {
    if (!newKeyHex.trim()) return;
    try {
      setSavingKey(true);
      const response = await setPrivateKey(newKeyHex.trim());
      if (response.success) {
        // Reload encryption status
        window.location.reload();
      } else {
        showError(response.message);
      }
    } catch (err) {
      showError(err instanceof Error ? err.message : 'Failed to save key');
    } finally {
      setSavingKey(false);
    }
  };

  const handleCopyCurrentKey = async () => {
    if (currentKeyHex) {
      await navigator.clipboard.writeText(currentKeyHex);
      setKeyCopied(true);
      setTimeout(() => setKeyCopied(false), 2000);
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
      await mutateSecrets();
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
      await mutateSecrets();
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
      await mutateSecrets();
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
      await mutateSecrets();
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
      await mutateSecrets();
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

  const loading = authLoading || encryptionLoading || secretsLoading;

  const hasSecrets = (secretsStatus?.registries ?? []).some(r => r.secret_count > 0);

  return (
    <div className="flex-1 p-6 overflow-auto">
      <div className="max-w-4xl mx-auto space-y-8">
      {/* Header */}
      <div>
        <h1 className="text-2xl font-semibold text-white mb-2">Security</h1>
        <p className="text-white/50">
          Authentication, encryption, and secrets management.
        </p>
      </div>

      {loading ? (
        <div className="space-y-4">
          {Array.from({ length: 3 }).map((_, index) => (
            <div key={index} className="rounded-xl border border-white/[0.06] bg-white/[0.02] p-6">
              <div className="flex items-center gap-3 mb-5">
                <div className="h-9 w-9 rounded-lg bg-white/[0.06]" />
                <div className="space-y-2">
                  <div className="h-4 w-36 rounded bg-white/[0.06]" />
                  <div className="h-3 w-52 rounded bg-white/[0.04]" />
                </div>
              </div>
              <div className="grid grid-cols-1 sm:grid-cols-2 gap-4">
                <div className="h-20 rounded-lg bg-white/[0.03] border border-white/[0.04]" />
                <div className="h-20 rounded-lg bg-white/[0.03] border border-white/[0.04]" />
              </div>
            </div>
          ))}
        </div>
      ) : (
      <>
      {/* Auth Status Card */}
      <div className="rounded-xl border border-white/[0.06] bg-white/[0.02] p-6">
        <div className="flex items-center gap-3 mb-4">
          <div className="flex h-9 w-9 items-center justify-center rounded-lg bg-white/[0.06]">
            <Shield className="h-5 w-5 text-white/70" />
          </div>
          <div>
            <h2 className="text-base font-medium text-white">Authentication</h2>
            <p className="text-xs text-white/40">Current authentication configuration</p>
          </div>
        </div>

        <div className="grid grid-cols-1 sm:grid-cols-2 gap-4 mb-6">
          <div className="rounded-lg bg-white/[0.03] border border-white/[0.04] p-4">
            <p className="text-xs text-white/40 mb-1">Auth Mode</p>
            <span className={`inline-flex items-center gap-1.5 rounded-full px-2.5 py-0.5 text-xs font-medium ${
              authStatus?.auth_mode === 'disabled'
                ? 'bg-yellow-500/10 text-yellow-400'
                : authStatus?.auth_mode === 'multi_user'
                ? 'bg-blue-500/10 text-blue-400'
                : 'bg-emerald-500/10 text-emerald-400'
            }`}>
              {authModeLabel}
            </span>
          </div>

          <div className="rounded-lg bg-white/[0.03] border border-white/[0.04] p-4">
            <p className="text-xs text-white/40 mb-1">Password Source</p>
            <span className={`inline-flex items-center gap-1.5 rounded-full px-2.5 py-0.5 text-xs font-medium ${
              authStatus?.password_source === 'dashboard'
                ? 'bg-emerald-500/10 text-emerald-400'
                : authStatus?.password_source === 'environment'
                ? 'bg-blue-500/10 text-blue-400'
                : 'bg-white/[0.06] text-white/50'
            }`}>
              {passwordSourceLabel}
            </span>
          </div>

          {authStatus?.password_changed_at && (
            <div className="rounded-lg bg-white/[0.03] border border-white/[0.04] p-4">
              <p className="text-xs text-white/40 mb-1">Last Changed</p>
              <p className="flex items-center gap-1.5 text-sm text-white/70">
                <Clock className="h-3.5 w-3.5" />
                {new Date(authStatus.password_changed_at).toLocaleString()}
              </p>
            </div>
          )}

          {authStatus?.dev_mode && (
            <div className="rounded-lg bg-yellow-500/5 border border-yellow-500/10 p-4">
              <p className="flex items-center gap-1.5 text-xs text-yellow-400">
                <AlertTriangle className="h-3.5 w-3.5" />
                Dev mode is enabled &mdash; authentication is bypassed
              </p>
            </div>
          )}
        </div>

        {/* Password Management */}
        {isMultiUser ? (
          <div className="rounded-lg bg-white/[0.03] border border-white/[0.04] p-4">
            <p className="flex items-center gap-2 text-sm text-white/50">
              <Info className="h-4 w-4 shrink-0" />
              In multi-user mode, passwords are managed via the <code className="mx-1 rounded bg-white/[0.06] px-1.5 py-0.5 text-xs font-mono">SANDBOXED_USERS</code> environment variable.
            </p>
          </div>
        ) : (
          <form onSubmit={handleChangePassword} className="space-y-4 max-w-md">
            {!hasExistingPassword && (
              <div className="rounded-lg bg-blue-500/5 border border-blue-500/10 p-3">
                <p className="flex items-center gap-2 text-xs text-blue-400">
                  <Info className="h-3.5 w-3.5 shrink-0" />
                  No password is configured. Set one to enable authentication.
                  {!authStatus?.dev_mode && ' You will also need JWT_SECRET set as an environment variable.'}
                </p>
              </div>
            )}

            {requireCurrentPassword && (
              <div>
                <label className="block text-sm font-medium text-white/70 mb-1.5">
                  Current Password
                </label>
                <input
                  type="password"
                  value={currentPassword}
                  onChange={(e) => setCurrentPassword(e.target.value)}
                  className="w-full rounded-lg border border-white/[0.08] bg-white/[0.04] px-3 py-2 text-sm text-white placeholder-white/30 focus:border-white/20 focus:outline-none focus:ring-1 focus:ring-white/20"
                  placeholder="Enter current password"
                  required
                />
              </div>
            )}

            <div>
              <label className="block text-sm font-medium text-white/70 mb-1.5">
                New Password
              </label>
              <input
                type="password"
                value={newPassword}
                onChange={(e) => setNewPassword(e.target.value)}
                className="w-full rounded-lg border border-white/[0.08] bg-white/[0.04] px-3 py-2 text-sm text-white placeholder-white/30 focus:border-white/20 focus:outline-none focus:ring-1 focus:ring-white/20"
                placeholder="At least 8 characters"
                minLength={8}
                required
              />
            </div>

            <div>
              <label className="block text-sm font-medium text-white/70 mb-1.5">
                Confirm New Password
              </label>
              <input
                type="password"
                value={confirmPassword}
                onChange={(e) => setConfirmPassword(e.target.value)}
                className="w-full rounded-lg border border-white/[0.08] bg-white/[0.04] px-3 py-2 text-sm text-white placeholder-white/30 focus:border-white/20 focus:outline-none focus:ring-1 focus:ring-white/20"
                placeholder="Confirm new password"
                minLength={8}
                required
              />
              {confirmPassword && newPassword !== confirmPassword && (
                <p className="mt-1 text-xs text-red-400">Passwords do not match</p>
              )}
            </div>

            <button
              type="submit"
              disabled={savingPassword || !newPassword || newPassword !== confirmPassword || newPassword.length < 8}
              className="inline-flex items-center gap-2 rounded-lg bg-white/[0.08] px-4 py-2 text-sm font-medium text-white transition-colors hover:bg-white/[0.12] disabled:opacity-40 disabled:cursor-not-allowed"
            >
              {savingPassword ? (
                <>
                  <span className="h-4 w-4 animate-spin rounded-full border-2 border-white/20 border-t-white/70" />
                  Saving...
                </>
              ) : (
                <>
                  <Lock className="h-4 w-4" />
                  {hasExistingPassword ? 'Update Password' : 'Set Password'}
                </>
              )}
            </button>

            {authStatus?.password_source === 'environment' && (
              <p className="text-xs text-white/30">
                Setting a dashboard password will take priority over the DASHBOARD_PASSWORD environment variable.
              </p>
            )}
          </form>
        )}
      </div>

      {/* Encryption Status Card */}
      <div className="rounded-xl bg-white/[0.02] border border-white/[0.06] p-6">
        <div className="flex items-start gap-4">
          <div className={cn(
            'w-12 h-12 rounded-xl flex items-center justify-center',
            encryptionStatus?.key_available ? 'bg-emerald-500/10' : 'bg-amber-500/10'
          )}>
            <FileKey className={cn(
              'h-6 w-6',
              encryptionStatus?.key_available ? 'text-emerald-400' : 'text-amber-400'
            )} />
          </div>
          <div className="flex-1">
            <div className="flex items-center gap-2 mb-1">
              <h2 className="text-lg font-medium text-white">Skill Content Encryption</h2>
              {encryptionStatus?.key_available ? (
                <span className="flex items-center gap-1 text-xs text-emerald-400 bg-emerald-500/10 px-2 py-0.5 rounded-full">
                  <CheckCircle className="h-3 w-3" />
                  Active
                </span>
              ) : (
                <span className="flex items-center gap-1 text-xs text-amber-400 bg-amber-500/10 px-2 py-0.5 rounded-full">
                  <AlertCircle className="h-3 w-3" />
                  Not Configured
                </span>
              )}
            </div>
            <p className="text-sm text-white/50 mb-3">
              Encrypts <code className="text-xs bg-white/[0.06] px-1 py-0.5 rounded">&lt;encrypted&gt;...&lt;/encrypted&gt;</code> tags in skill markdown files.
            </p>
            {encryptionStatus?.key_available ? (
              <div className="flex items-center justify-between">
                <div className="flex items-center gap-4 text-sm">
                  <div className="flex items-center gap-2 text-white/60">
                    {encryptionStatus.key_source === 'environment' ? (
                      <>
                        <Server className="h-4 w-4" />
                        <span>Key from environment variable</span>
                      </>
                    ) : (
                      <>
                        <FileKey className="h-4 w-4" />
                        <span>Key from file</span>
                      </>
                    )}
                  </div>
                  {encryptionStatus.key_file_path && encryptionStatus.key_source === 'file' && (
                    <code className="text-xs text-white/40 bg-white/[0.04] px-2 py-1 rounded">
                      {encryptionStatus.key_file_path}
                    </code>
                  )}
                </div>
                <button
                  onClick={handleOpenKeyDialog}
                  className="flex items-center gap-2 px-3 py-1.5 text-xs font-medium text-white/60 hover:text-white bg-white/[0.04] hover:bg-white/[0.08] rounded-lg transition-colors"
                >
                  <Settings className="h-3.5 w-3.5" />
                  Manage Key
                </button>
              </div>
            ) : (
              <div className="flex items-center justify-between">
                <p className="text-sm text-white/40">
                  Set <code className="text-xs bg-white/[0.06] px-1 py-0.5 rounded">PRIVATE_KEY</code> environment variable or configure below.
                </p>
                <button
                  onClick={handleOpenKeyDialog}
                  className="flex items-center gap-2 px-3 py-1.5 text-xs font-medium text-indigo-400 bg-indigo-500/10 hover:bg-indigo-500/20 rounded-lg transition-colors"
                >
                  <Settings className="h-3.5 w-3.5" />
                  Configure Key
                </button>
              </div>
            )}
          </div>
        </div>
      </div>

      {/* Secrets Store Section */}
      <div className="rounded-xl bg-white/[0.02] border border-white/[0.06] overflow-hidden">
        <div className="p-4 border-b border-white/[0.06] flex items-center justify-between">
          <div className="flex items-center gap-3">
            <div className="w-10 h-10 rounded-lg bg-white/[0.04] flex items-center justify-center">
              <Shield className="h-5 w-5 text-white/60" />
            </div>
            <div>
              <h2 className="text-base font-medium text-white">Secrets Store</h2>
              <p className="text-xs text-white/40">Optional key-value storage for credentials</p>
            </div>
          </div>
          {secretsStatus?.initialized && (
            <div className="flex items-center gap-2">
              {secretsStatus.can_decrypt ? (
                <button
                  onClick={handleLock}
                  className="flex items-center gap-2 px-3 py-1.5 text-xs font-medium text-amber-400 bg-amber-500/10 hover:bg-amber-500/20 rounded-lg transition-colors"
                >
                  <Lock className="h-3.5 w-3.5" />
                  Lock
                </button>
              ) : (
                <button
                  onClick={() => setShowUnlockDialog(true)}
                  className="flex items-center gap-2 px-3 py-1.5 text-xs font-medium text-emerald-400 bg-emerald-500/10 hover:bg-emerald-500/20 rounded-lg transition-colors"
                >
                  <Unlock className="h-3.5 w-3.5" />
                  Unlock
                </button>
              )}
              <button
                onClick={() => {
                  setNewSecretRegistry(selectedRegistry || 'mcp-tokens');
                  setShowAddDialog(true);
                }}
                disabled={!secretsStatus.can_decrypt}
                className="flex items-center gap-2 px-3 py-1.5 text-xs font-medium text-white bg-indigo-500 hover:bg-indigo-600 rounded-lg transition-colors disabled:opacity-50 disabled:cursor-not-allowed"
              >
                <Plus className="h-3.5 w-3.5" />
                Add Secret
              </button>
            </div>
          )}
        </div>

        {!secretsStatus?.initialized ? (
          <div className="p-8 text-center">
            <p className="text-sm text-white/50 mb-4">
              The secrets store is not initialized. This is optional and separate from skill encryption.
            </p>
            <button
              onClick={() => setShowInitDialog(true)}
              className="px-4 py-2 text-sm font-medium text-white/70 border border-white/[0.08] hover:bg-white/[0.04] rounded-lg transition-colors"
            >
              Initialize Secrets Store
            </button>
          </div>
        ) : !secretsStatus.can_decrypt ? (
          <div className="p-8 text-center">
            <Lock className="h-8 w-8 text-white/20 mx-auto mb-3" />
            <p className="text-sm text-white/50 mb-4">
              Secrets store is locked. Enter passphrase to access.
            </p>
            <button
              onClick={() => setShowUnlockDialog(true)}
              className="px-4 py-2 text-sm font-medium text-emerald-400 bg-emerald-500/10 hover:bg-emerald-500/20 rounded-lg transition-colors"
            >
              Unlock
            </button>
          </div>
        ) : !hasSecrets ? (
          <div className="p-8 text-center">
            <p className="text-sm text-white/50">
              No secrets stored. Click &quot;Add Secret&quot; to store credentials.
            </p>
          </div>
        ) : (
          <div className="flex">
            {/* Registries sidebar */}
            <div className="w-48 border-r border-white/[0.06] p-2">
              {secretsStatus.registries.map((registry) => (
                <button
                  key={registry.name}
                  onClick={() => setSelectedRegistry(registry.name)}
                  className={cn(
                    'w-full text-left p-2 rounded-lg transition-colors mb-1 text-sm',
                    selectedRegistry === registry.name
                      ? 'bg-white/[0.08] text-white'
                      : 'text-white/60 hover:bg-white/[0.04] hover:text-white'
                  )}
                >
                  <div className="flex items-center gap-2">
                    <Key className="h-3.5 w-3.5" />
                    <span className="truncate">{registry.name}</span>
                  </div>
                  <p className="text-xs text-white/40 mt-0.5 ml-5">
                    {registry.secret_count} secret{registry.secret_count !== 1 ? 's' : ''}
                  </p>
                </button>
              ))}
            </div>

            {/* Secrets list */}
            <div className="flex-1">
              <div className="divide-y divide-white/[0.06]">
                {loadingSecrets ? (
                  <div className="p-8 flex items-center justify-center">
                    <Loader className="h-5 w-5 animate-spin text-white/40" />
                  </div>
                ) : secrets.length === 0 ? (
                  <div className="p-8 text-center text-white/40 text-sm">
                    No secrets in this registry
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
        )}
      </div>
      </>
      )}
      </div>

      {/* Initialize Dialog */}
      {showInitDialog && (
        <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/50">
          <div className="w-full max-w-md p-6 rounded-xl bg-[#1a1a1c] border border-white/[0.06]">
            <h3 className="text-lg font-medium text-white mb-4">Initialize Secrets Store</h3>
            <p className="text-sm text-white/60 mb-4">
              This creates a separate encrypted key-value store. After initialization, set{' '}
              <code className="px-1 py-0.5 rounded bg-white/[0.06] text-amber-400">
                OPENAGENT_SECRET_PASSPHRASE
              </code>{' '}
              to enable encryption.
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
                  placeholder="e.g., service/api_key"
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

      {/* Private Key Management Dialog */}
      {showKeyDialog && (
        <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/50">
          <div className="w-full max-w-lg p-6 rounded-xl bg-[#1a1a1c] border border-white/[0.06]">
            <h3 className="text-lg font-medium text-white mb-4">Manage Encryption Key</h3>

            {loadingKey ? (
              <div className="flex items-center justify-center py-8">
                <Loader className="h-6 w-6 animate-spin text-white/40" />
              </div>
            ) : (
              <div className="space-y-4">
                {/* Current key section */}
                {currentKeyHex && (
                  <div>
                    <label className="block text-sm text-white/60 mb-2">Current Key (hex)</label>
                    <div className="flex gap-2">
                      <div className="flex-1 relative">
                        <input
                          type={showCurrentKey ? 'text' : 'password'}
                          value={currentKeyHex}
                          readOnly
                          className="w-full px-4 py-2 rounded-lg bg-white/[0.04] border border-white/[0.08] text-white font-mono text-sm focus:outline-none"
                        />
                        <button
                          onClick={() => setShowCurrentKey(!showCurrentKey)}
                          className="absolute right-2 top-1/2 -translate-y-1/2 p-1 text-white/40 hover:text-white"
                        >
                          {showCurrentKey ? <EyeOff className="h-4 w-4" /> : <Eye className="h-4 w-4" />}
                        </button>
                      </div>
                      <button
                        onClick={handleCopyCurrentKey}
                        className="px-3 py-2 rounded-lg bg-white/[0.04] hover:bg-white/[0.08] text-white/60 hover:text-white transition-colors"
                        title="Copy key"
                      >
                        {keyCopied ? <Check className="h-4 w-4 text-emerald-400" /> : <Copy className="h-4 w-4" />}
                      </button>
                    </div>
                  </div>
                )}

                {/* Mode toggle */}
                <div className="flex gap-2 border-b border-white/[0.06] pb-4">
                  <button
                    onClick={() => setKeyDialogMode('view')}
                    className={cn(
                      'px-3 py-1.5 text-sm rounded-lg transition-colors',
                      keyDialogMode === 'view'
                        ? 'bg-white/[0.08] text-white'
                        : 'text-white/40 hover:text-white'
                    )}
                  >
                    View
                  </button>
                  <button
                    onClick={() => setKeyDialogMode('edit')}
                    className={cn(
                      'px-3 py-1.5 text-sm rounded-lg transition-colors',
                      keyDialogMode === 'edit'
                        ? 'bg-white/[0.08] text-white'
                        : 'text-white/40 hover:text-white'
                    )}
                  >
                    {currentKeyHex ? 'Update Key' : 'Set Key'}
                  </button>
                </div>

                {/* Edit mode */}
                {keyDialogMode === 'edit' && (
                  <div>
                    <label className="block text-sm text-white/60 mb-2">
                      {currentKeyHex ? 'New Key (hex, 64 characters)' : 'Enter Key (hex, 64 characters)'}
                    </label>
                    <textarea
                      placeholder="Enter 64-character hex key (256 bits)..."
                      value={newKeyHex}
                      onChange={(e) => setNewKeyHex(e.target.value)}
                      rows={2}
                      className="w-full px-4 py-2 rounded-lg bg-white/[0.04] border border-white/[0.08] text-white placeholder:text-white/30 focus:outline-none focus:border-indigo-500/50 resize-none font-mono text-sm"
                    />
                    {currentKeyHex && (
                      <p className="text-xs text-amber-400/80 mt-2">
                        Warning: Changing the key will re-encrypt all skill content that can be decrypted with the current key.
                      </p>
                    )}
                    {newKeyHex && newKeyHex.length !== 64 && (
                      <p className="text-xs text-red-400/80 mt-2">
                        Key must be exactly 64 hex characters ({newKeyHex.length}/64)
                      </p>
                    )}
                  </div>
                )}

                {/* Info section for view mode */}
                {keyDialogMode === 'view' && !currentKeyHex && (
                  <div className="py-4 text-center">
                    <p className="text-sm text-white/50 mb-2">No encryption key configured.</p>
                    <p className="text-xs text-white/40">
                      Click &quot;Set Key&quot; to configure an encryption key for skill content.
                    </p>
                  </div>
                )}
              </div>
            )}

            <div className="flex justify-end gap-2 mt-6">
              <button
                onClick={() => {
                  setShowKeyDialog(false);
                  setNewKeyHex('');
                  setShowCurrentKey(false);
                }}
                className="px-4 py-2 text-sm text-white/60 hover:text-white"
              >
                {keyDialogMode === 'view' ? 'Close' : 'Cancel'}
              </button>
              {keyDialogMode === 'edit' && (
                <button
                  onClick={handleSaveKey}
                  disabled={!newKeyHex.trim() || newKeyHex.length !== 64 || savingKey}
                  className="px-4 py-2 text-sm font-medium text-white bg-indigo-500 hover:bg-indigo-600 rounded-lg disabled:opacity-50"
                >
                  {savingKey ? 'Saving...' : currentKeyHex ? 'Update Key' : 'Set Key'}
                </button>
              )}
            </div>
          </div>
        </div>
      )}
    </div>
  );
}
