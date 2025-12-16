'use client';

import { useState, useEffect, useCallback } from 'react';
import { toast } from 'sonner';
import { getHealth, HealthResponse } from '@/lib/api';
import { Server, Bot, Cpu, Wallet, Save, RefreshCw, AlertTriangle } from 'lucide-react';
import { readSavedSettings, writeSavedSettings } from '@/lib/settings';
import { cn } from '@/lib/utils';

export default function SettingsPage() {
  const [health, setHealth] = useState<HealthResponse | null>(null);
  const [healthLoading, setHealthLoading] = useState(true);
  const [testingConnection, setTestingConnection] = useState(false);
  
  // Form state
  const [apiUrl, setApiUrl] = useState(() => readSavedSettings().apiUrl ?? 'http://127.0.0.1:3000');
  const [defaultModel, setDefaultModel] = useState(
    () => readSavedSettings().defaultModel ?? 'anthropic/claude-sonnet-4.5'
  );
  const [defaultBudget, setDefaultBudget] = useState(() => readSavedSettings().defaultBudget ?? '1000');
  
  // Track original values for unsaved changes
  const [originalValues, setOriginalValues] = useState({
    apiUrl: readSavedSettings().apiUrl ?? 'http://127.0.0.1:3000',
    defaultModel: readSavedSettings().defaultModel ?? 'anthropic/claude-sonnet-4.5',
    defaultBudget: readSavedSettings().defaultBudget ?? '1000',
  });
  
  // Validation state
  const [urlError, setUrlError] = useState<string | null>(null);
  const [budgetError, setBudgetError] = useState<string | null>(null);
  
  // Check if there are unsaved changes
  const hasUnsavedChanges = apiUrl !== originalValues.apiUrl || 
    defaultModel !== originalValues.defaultModel || 
    defaultBudget !== originalValues.defaultBudget;

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

  // Validate budget
  const validateBudget = useCallback((budget: string) => {
    const num = parseInt(budget, 10);
    if (isNaN(num) || num < 0) {
      setBudgetError('Budget must be a positive number');
      return false;
    }
    if (num > 1000000) {
      setBudgetError('Budget seems too high (max $10,000)');
      return false;
    }
    setBudgetError(null);
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
  }, [apiUrl, defaultModel, defaultBudget]);

  const handleSave = () => {
    const urlValid = validateUrl(apiUrl);
    const budgetValid = validateBudget(defaultBudget);
    
    if (!urlValid || !budgetValid) {
      toast.error('Please fix validation errors before saving');
      return;
    }

    writeSavedSettings({ apiUrl, defaultModel, defaultBudget });
    setOriginalValues({ apiUrl, defaultModel, defaultBudget });
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

          {/* Default Model */}
          <div className="rounded-xl bg-white/[0.02] border border-white/[0.04] p-5">
            <div className="flex items-center gap-3 mb-4">
              <div className="flex h-10 w-10 items-center justify-center rounded-xl bg-indigo-500/10">
                <Cpu className="h-5 w-5 text-indigo-400" />
              </div>
              <div>
                <h2 className="text-sm font-medium text-white">Default Model</h2>
                <p className="text-xs text-white/40">Choose the AI model for tasks</p>
              </div>
            </div>

            <div>
              <label className="block text-xs font-medium text-white/60 mb-1.5">
                Model ID
              </label>
              <select
                value={defaultModel}
                onChange={(e) => setDefaultModel(e.target.value)}
                className="w-full rounded-lg border border-white/[0.06] bg-white/[0.02] px-3 py-2.5 text-sm text-white focus:border-indigo-500/50 focus:outline-none transition-colors"
              >
                <option value="anthropic/claude-sonnet-4.5">Claude Sonnet 4.5 (Recommended)</option>
                <option value="anthropic/claude-haiku-4.5">Claude Haiku 4.5 (Budget)</option>
                <option value="anthropic/claude-opus-4.5">Claude Opus 4.5 (Premium)</option>
                <option value="openai/gpt-4o">GPT-4o</option>
                <option value="openai/gpt-4o-mini">GPT-4o Mini</option>
              </select>
            </div>
          </div>

          {/* Budget */}
          <div className="rounded-xl bg-white/[0.02] border border-white/[0.04] p-5">
            <div className="flex items-center gap-3 mb-4">
              <div className="flex h-10 w-10 items-center justify-center rounded-xl bg-indigo-500/10">
                <Wallet className="h-5 w-5 text-indigo-400" />
              </div>
              <div>
                <h2 className="text-sm font-medium text-white">Default Budget</h2>
                <p className="text-xs text-white/40">Maximum spend per task</p>
              </div>
            </div>

            <div>
              <label className="block text-xs font-medium text-white/60 mb-1.5">
                Budget per task (cents)
              </label>
              <input
                type="number"
                value={defaultBudget}
                onChange={(e) => {
                  setDefaultBudget(e.target.value);
                  validateBudget(e.target.value);
                }}
                className={cn(
                  "w-full rounded-lg border bg-white/[0.02] px-3 py-2.5 text-sm text-white placeholder-white/30 focus:outline-none transition-colors",
                  budgetError 
                    ? "border-red-500/50 focus:border-red-500/50" 
                    : "border-white/[0.06] focus:border-indigo-500/50"
                )}
              />
              {budgetError ? (
                <p className="mt-1.5 text-xs text-red-400">{budgetError}</p>
              ) : (
                <p className="mt-1.5 text-xs text-white/30">
                  {defaultBudget ? `$${(parseInt(defaultBudget) / 100).toFixed(2)}` : '$0.00'} per task
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
            disabled={!!urlError || !!budgetError}
            className={cn(
              "w-full flex items-center justify-center gap-2 rounded-lg px-4 py-2.5 text-sm font-medium text-white transition-colors",
              urlError || budgetError
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
