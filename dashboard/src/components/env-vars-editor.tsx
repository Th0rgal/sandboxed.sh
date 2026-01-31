'use client';

import { Eye, EyeOff, FileText, X, Lock, Unlock, AlertTriangle } from 'lucide-react';
import { cn } from '@/lib/utils';

/** Marker prefix for values that failed to decrypt */
const DECRYPTION_FAILED_PREFIX = '[DECRYPTION_FAILED]';

export type EnvRow = {
  id: string;
  key: string;
  value: string;
  secret: boolean;
  visible: boolean;
  encrypted: boolean;
  /** True if this value failed to decrypt (wrong key or missing key) */
  decryptionFailed: boolean;
};

const SENSITIVE_PATTERNS = [
  'KEY', 'TOKEN', 'SECRET', 'PASSWORD', 'PASS', 'CREDENTIAL', 'AUTH',
  'PRIVATE', 'API_KEY', 'ACCESS_TOKEN', 'B64', 'BASE64', 'ENCRYPTED',
];

export const isSensitiveKey = (key: string): boolean => {
  const upperKey = key.toUpperCase();
  return SENSITIVE_PATTERNS.some(pattern => upperKey.includes(pattern));
};

export const toEnvRows = (env: Record<string, string>, encryptedKeys?: string[]): EnvRow[] =>
  Object.entries(env).map(([key, value]) => {
    const secret = isSensitiveKey(key);
    // Check if value failed to decrypt
    const decryptionFailed = value.startsWith(DECRYPTION_FAILED_PREFIX);
    // If encryptedKeys is provided and non-empty, use it as the source of truth.
    // Otherwise fall back to auto-detection based on key name patterns (secret).
    // This ensures sensitive keys show as "will be encrypted" by default.
    const encrypted = (encryptedKeys && encryptedKeys.length > 0)
      ? encryptedKeys.includes(key)
      : secret;
    return {
      id: `${key}-${Math.random().toString(36).slice(2, 8)}`,
      key,
      // Strip the failed prefix from display - user needs to re-enter the value
      value: decryptionFailed ? '' : value,
      secret,
      visible: !(secret || encrypted), // Hide if secret OR encrypted
      encrypted,
      decryptionFailed,
    };
  });

export const envRowsToMap = (rows: EnvRow[]): Record<string, string> => {
  const env: Record<string, string> = {};
  rows.forEach((row) => {
    const key = row.key.trim();
    if (!key) return;
    env[key] = row.value;
  });
  return env;
};

export const getEncryptedKeys = (rows: EnvRow[]): string[] =>
  rows.filter((row) => row.encrypted && row.key.trim()).map((row) => row.key.trim());

/** Check if any rows have decryption failures */
export const hasDecryptionFailures = (rows: EnvRow[]): boolean =>
  rows.some((row) => row.decryptionFailed);

export const createEmptyEnvRow = (): EnvRow => ({
  id: Math.random().toString(36).slice(2),
  key: '',
  value: '',
  secret: false,
  visible: true,
  encrypted: false,
  decryptionFailed: false,
});

interface EnvVarsEditorProps {
  rows: EnvRow[];
  onChange: (rows: EnvRow[]) => void;
  className?: string;
  description?: string;
  /** Show encryption toggle per row. Only enable for templates which persist encrypted_keys. */
  showEncryptionToggle?: boolean;
}

export function EnvVarsEditor({ rows, onChange, className, description, showEncryptionToggle = false }: EnvVarsEditorProps) {
  const handleAddRow = () => {
    onChange([...rows, createEmptyEnvRow()]);
  };

  const handleRemoveRow = (id: string) => {
    onChange(rows.filter((r) => r.id !== id));
  };

  const handleKeyChange = (id: string, newKey: string) => {
    const newSecret = isSensitiveKey(newKey);
    onChange(
      rows.map((r) => {
        if (r.id !== id) return r;
        // When key changes and becomes sensitive, auto-enable encryption
        const newEncrypted = newSecret ? true : r.encrypted;
        return { ...r, key: newKey, secret: newSecret, visible: newSecret ? r.visible : true, encrypted: newEncrypted, decryptionFailed: false };
      })
    );
  };

  const handleToggleEncrypted = (id: string) => {
    onChange(rows.map((r) => {
      if (r.id !== id) return r;
      const newEncrypted = !r.encrypted;
      // When enabling encryption, hide the value
      return { ...r, encrypted: newEncrypted, visible: newEncrypted ? false : r.visible };
    }));
  };

  const handleValueChange = (id: string, newValue: string) => {
    onChange(rows.map((r) => (r.id === id ? { ...r, value: newValue, decryptionFailed: false } : r)));
  };

  const handleToggleVisibility = (id: string) => {
    onChange(rows.map((r) => (r.id === id ? { ...r, visible: !r.visible } : r)));
  };

  return (
    <div className={cn("rounded-xl bg-white/[0.02] border border-white/[0.05] overflow-hidden flex flex-col", className)}>
      <div className="px-4 py-3 border-b border-white/[0.05] flex items-center justify-between flex-shrink-0">
        <div className="flex items-center gap-2">
          <FileText className="h-4 w-4 text-indigo-400" />
          <p className="text-xs text-white/50 font-medium">Environment Variables</p>
        </div>
        <button
          onClick={handleAddRow}
          className="text-xs text-indigo-400 hover:text-indigo-300 font-medium"
        >
          + Add
        </button>
      </div>
      <div className="p-4 flex flex-col flex-1 min-h-0">
        {rows.length === 0 ? (
          <div className="py-6 text-center flex-1 flex flex-col items-center justify-center">
            <p className="text-xs text-white/40">No environment variables</p>
            <button
              onClick={handleAddRow}
              className="mt-3 text-xs text-indigo-400 hover:text-indigo-300"
            >
              Add first variable
            </button>
          </div>
        ) : (
          <div className="space-y-2 flex-1 overflow-y-auto min-h-[200px]">
            {rows.map((row) => (
              <div key={row.id} className="flex items-center gap-2">
                {showEncryptionToggle && (
                  <button
                    type="button"
                    onClick={() => handleToggleEncrypted(row.id)}
                    className={cn(
                      "p-2 rounded-lg transition-colors",
                      row.encrypted
                        ? "text-amber-400 hover:text-amber-300 hover:bg-amber-500/10"
                        : "text-white/30 hover:text-white/50 hover:bg-white/[0.06]"
                    )}
                    title={row.encrypted ? "Encrypted at rest (click to disable)" : "Not encrypted (click to enable)"}
                  >
                    {row.encrypted ? (
                      <Lock className="h-3.5 w-3.5" />
                    ) : (
                      <Unlock className="h-3.5 w-3.5" />
                    )}
                  </button>
                )}
                {row.decryptionFailed && (
                  <div
                    className="p-2 text-red-400"
                    title="Decryption failed - please re-enter this value"
                  >
                    <AlertTriangle className="h-3.5 w-3.5" />
                  </div>
                )}
                <input
                  value={row.key}
                  onChange={(e) => handleKeyChange(row.id, e.target.value)}
                  placeholder="KEY"
                  className={cn(
                    "flex-1 px-3 py-2 rounded-lg bg-black/20 border text-xs text-white placeholder:text-white/30 focus:outline-none",
                    row.decryptionFailed
                      ? "border-red-500/50 focus:border-red-500"
                      : "border-white/[0.06] focus:border-indigo-500/50"
                  )}
                />
                <span className="text-white/20">=</span>
                <div className="flex-1 relative">
                  <input
                    type={(row.encrypted || row.secret) && !row.visible ? 'password' : 'text'}
                    value={row.value}
                    onChange={(e) => handleValueChange(row.id, e.target.value)}
                    placeholder={row.decryptionFailed ? "Re-enter value (decryption failed)" : "value"}
                    className={cn(
                      "w-full px-3 py-2 rounded-lg bg-black/20 border text-xs text-white placeholder:text-white/30 focus:outline-none",
                      row.decryptionFailed
                        ? "border-red-500/50 focus:border-red-500 placeholder:text-red-400/50"
                        : "border-white/[0.06] focus:border-indigo-500/50",
                      (row.encrypted || row.secret) && "pr-8"
                    )}
                  />
                  {(row.encrypted || row.secret) && (
                    <button
                      type="button"
                      onClick={() => handleToggleVisibility(row.id)}
                      className="absolute right-2 top-1/2 -translate-y-1/2 text-white/30 hover:text-white/60 transition-colors"
                    >
                      {row.visible ? (
                        <EyeOff className="h-3.5 w-3.5" />
                      ) : (
                        <Eye className="h-3.5 w-3.5" />
                      )}
                    </button>
                  )}
                </div>
                <button
                  onClick={() => handleRemoveRow(row.id)}
                  className="p-2 rounded-lg text-white/40 hover:text-white/70 hover:bg-white/[0.06] transition-colors"
                >
                  <X className="h-3.5 w-3.5" />
                </button>
              </div>
            ))}
          </div>
        )}
        {rows.length > 0 && description && (
          <p className="text-xs text-white/35 mt-4 pt-3 border-t border-white/[0.04] flex-shrink-0">
            {description}
          </p>
        )}
      </div>
    </div>
  );
}
