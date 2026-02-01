'use client';

import { useState, useEffect, useCallback } from 'react';
import useSWR from 'swr';
import { toast } from '@/components/toast';
import { getHealth } from '@/lib/api';
import { Save } from 'lucide-react';
import { readSavedSettings, writeSavedSettings } from '@/lib/settings';
import { ServerConnectionCard } from '@/components/server-connection-card';

export default function SystemSettingsPage() {
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

  // SWR: fetch health status
  const { data: health, isLoading: healthLoading, mutate: mutateHealth } = useSWR(
    'health',
    getHealth,
    { revalidateOnFocus: false }
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

  // Test API connection
  const testApiConnection = async () => {
    if (!validateUrl(apiUrl)) return;
    setTestingConnection(true);
    try {
      await mutateHealth();
      toast.success('Connection successful!');
    } catch {
      toast.error('Failed to connect to server');
    } finally {
      setTestingConnection(false);
    }
  };

  // Save settings
  const handleSave = () => {
    if (!validateUrl(apiUrl)) return;

    writeSavedSettings({ apiUrl });
    setOriginalValues({ apiUrl });
    toast.success('Settings saved!');
  };

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

  return (
    <div className="flex-1 overflow-auto">
      <div className="max-w-4xl mx-auto px-6 py-8">
        {/* Header */}
        <div className="mb-8">
          <h1 className="text-2xl font-semibold text-white">System</h1>
          <p className="mt-1 text-sm text-white/50">
            Server connection settings
          </p>
        </div>

        {/* Content */}
        <div className="space-y-6">
          {/* Server Connection */}
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

          {/* Save Button */}
          <div className="flex items-center justify-end gap-3">
            {hasUnsavedChanges && (
              <span className="text-xs text-amber-400">Unsaved changes</span>
            )}
            <button
              onClick={handleSave}
              disabled={!hasUnsavedChanges || !!urlError}
              className="flex items-center gap-2 rounded-lg bg-indigo-500 px-4 py-2 text-sm font-medium text-white hover:bg-indigo-600 transition-colors disabled:opacity-50 disabled:cursor-not-allowed"
            >
              <Save className="h-4 w-4" />
              Save Changes
            </button>
          </div>
        </div>
      </div>
    </div>
  );
}
