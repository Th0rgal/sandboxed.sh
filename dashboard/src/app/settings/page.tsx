'use client';

import { useState, useEffect, useCallback } from 'react';
import { toast } from 'sonner';
import { getHealth, HealthResponse } from '@/lib/api';
import { Server, Bot, Save, RefreshCw, AlertTriangle, GitBranch } from 'lucide-react';
import { readSavedSettings, writeSavedSettings } from '@/lib/settings';
import { cn } from '@/lib/utils';

export default function SettingsPage() {
  const [health, setHealth] = useState<HealthResponse | null>(null);
  const [healthLoading, setHealthLoading] = useState(true);
  const [testingConnection, setTestingConnection] = useState(false);
  
  // Form state
  const [apiUrl, setApiUrl] = useState(() => readSavedSettings().apiUrl ?? 'http://127.0.0.1:3000');
  const [libraryRepo, setLibraryRepo] = useState(() => readSavedSettings().libraryRepo ?? '');
  
  // Track original values for unsaved changes
  const [originalValues, setOriginalValues] = useState({
    apiUrl: readSavedSettings().apiUrl ?? 'http://127.0.0.1:3000',
    libraryRepo: readSavedSettings().libraryRepo ?? '',
  });
  
  // Validation state
  const [urlError, setUrlError] = useState<string | null>(null);
  const [repoError, setRepoError] = useState<string | null>(null);
  
  // Check if there are unsaved changes
  const hasUnsavedChanges = apiUrl !== originalValues.apiUrl || 
    libraryRepo !== originalValues.libraryRepo;

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

  // Check health on mount
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

  const testConnection = async () => {
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
      toast.error(`Connection failed: ${err instanceof Error ? err.message : 'Unknown error'}`);
    } finally {
      setTestingConnection(false);
    }
  };

  return (
    <div className="min-h-screen flex flex-col items-center p-6">
      {/* Centered content container */}
      <div className="w-full max-w-xl">
        {/* Header */}
        <div className="mb-8 flex items-start justify-between">
          <div>
            <h1 className="text-xl font-semibold text-white">Settings</h1>
            <p className="mt-1 text-sm text-white/50">Configure your server connection and preferences</p>
          </div>
          {hasUnsavedChanges && (
            <div className="flex items-center gap-2 text-amber-400 text-xs">
              <AlertTriangle className="h-3.5 w-3.5" />
              <span>Unsaved changes</span>
            </div>
          )}
        </div>

        <div className="space-y-5">
          {/* Connection Status */}
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
                    "w-full rounded-lg border bg-white/[0.02] px-3 py-2.5 text-sm text-white placeholder-white/30 focus:outline-none transition-colors",
                    urlError 
                      ? "border-red-500/50 focus:border-red-500/50" 
                      : "border-white/[0.06] focus:border-indigo-500/50"
                  )}
                />
                {urlError && (
                  <p className="mt-1.5 text-xs text-red-400">{urlError}</p>
                )}
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
                  onClick={testConnection}
                  disabled={testingConnection}
                  className="flex items-center gap-2 rounded-lg border border-white/[0.06] bg-white/[0.02] px-3 py-1.5 text-xs text-white/70 hover:bg-white/[0.04] transition-colors disabled:opacity-50"
                >
                  <RefreshCw className={cn("h-3 w-3", testingConnection && "animate-spin")} />
                  Test Connection
                </button>
              </div>
            </div>
          </div>

          {/* Configuration Library */}
          <div className="rounded-xl bg-white/[0.02] border border-white/[0.04] p-5">
            <div className="flex items-center gap-3 mb-4">
              <div className="flex h-10 w-10 items-center justify-center rounded-xl bg-indigo-500/10">
                <GitBranch className="h-5 w-5 text-indigo-400" />
              </div>
              <div>
                <h2 className="text-sm font-medium text-white">Configuration Library</h2>
                <p className="text-xs text-white/40">Git repo for MCPs, skills, and commands</p>
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
                  "w-full rounded-lg border bg-white/[0.02] px-3 py-2.5 text-sm text-white placeholder-white/30 focus:outline-none transition-colors",
                  repoError 
                    ? "border-red-500/50 focus:border-red-500/50" 
                    : "border-white/[0.06] focus:border-indigo-500/50"
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

          {/* About */}
          <div className="rounded-xl bg-white/[0.02] border border-white/[0.04] p-5">
            <div className="flex items-center gap-3 mb-4">
              <div className="flex h-10 w-10 items-center justify-center rounded-xl bg-indigo-500/10">
                <Bot className="h-5 w-5 text-indigo-400" />
              </div>
              <div>
                <h2 className="text-sm font-medium text-white">About OpenAgent</h2>
                <p className="text-xs text-white/40">Autonomous coding agent</p>
              </div>
            </div>

            <div className="grid grid-cols-2 gap-2">
              <div className="flex items-center gap-2 rounded-lg bg-white/[0.02] px-3 py-2">
                <span className="h-1.5 w-1.5 rounded-full bg-indigo-400" />
                <span className="text-xs text-white/60">AI-maintainable</span>
              </div>
              <div className="flex items-center gap-2 rounded-lg bg-white/[0.02] px-3 py-2">
                <span className="h-1.5 w-1.5 rounded-full bg-emerald-400" />
                <span className="text-xs text-white/60">Self-contained</span>
              </div>
              <div className="flex items-center gap-2 rounded-lg bg-white/[0.02] px-3 py-2">
                <span className="h-1.5 w-1.5 rounded-full bg-amber-400" />
                <span className="text-xs text-white/60">Full-access</span>
              </div>
              <div className="flex items-center gap-2 rounded-lg bg-white/[0.02] px-3 py-2">
                <span className="h-1.5 w-1.5 rounded-full bg-blue-400" />
                <span className="text-xs text-white/60">Hierarchical</span>
              </div>
            </div>
          </div>

          {/* Save Button */}
          <button
            onClick={handleSave}
            disabled={!!urlError || !!repoError}
            className={cn(
              "w-full flex items-center justify-center gap-2 rounded-lg px-4 py-2.5 text-sm font-medium text-white transition-colors",
              urlError || repoError
                ? "bg-white/10 cursor-not-allowed opacity-50"
                : "bg-indigo-500 hover:bg-indigo-600"
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
