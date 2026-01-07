'use client';

import { useState, useEffect, useRef } from 'react';
import {
  GitBranch,
  ArrowRight,
  Loader,
  Key,
  ExternalLink,
  ChevronLeft,
  Search,
  Plus,
  Lock,
  Globe,
  CheckCircle,
} from 'lucide-react';
import { readSavedSettings, writeSavedSettings } from '@/lib/settings';
import { getLibraryStatus } from '@/lib/api';
import { cn } from '@/lib/utils';

type LibraryUnavailableProps = {
  message?: string | null;
  onConfigured?: () => void;
};

type GitHubRepo = {
  id: number;
  name: string;
  full_name: string;
  clone_url: string;
  ssh_url: string;
  private: boolean;
  description: string | null;
  updated_at: string;
};

type Step = 'token' | 'select' | 'create' | 'connecting';

export function LibraryUnavailable({ message, onConfigured }: LibraryUnavailableProps) {
  const details = message?.trim();
  const showDetails = !!details && details !== 'Library not initialized';

  const [step, setStep] = useState<Step>('token');
  const [token, setToken] = useState('');
  const [repos, setRepos] = useState<GitHubRepo[]>([]);
  const [loading, setLoading] = useState(false);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [searchQuery, setSearchQuery] = useState('');
  const [selectedRepo, setSelectedRepo] = useState<GitHubRepo | null>(null);

  // Create repo state
  const [newRepoName, setNewRepoName] = useState('openagent-library');
  const [newRepoPrivate, setNewRepoPrivate] = useState(true);
  const [creating, setCreating] = useState(false);

  // Connecting state
  const [connectingRepoName, setConnectingRepoName] = useState('');
  const [connectingStatus, setConnectingStatus] = useState('Initializing library...');
  const retryRef = useRef<NodeJS.Timeout | null>(null);

  // Cleanup on unmount
  useEffect(() => {
    return () => {
      if (retryRef.current) {
        clearTimeout(retryRef.current);
      }
    };
  }, []);

  const fetchRepos = async () => {
    if (!token.trim()) {
      setError('Please enter a GitHub token');
      return;
    }

    setLoading(true);
    setError(null);

    try {
      const response = await fetch('https://api.github.com/user/repos?per_page=100&sort=updated', {
        headers: {
          Authorization: `Bearer ${token.trim()}`,
          Accept: 'application/vnd.github+json',
        },
      });

      if (!response.ok) {
        if (response.status === 401) {
          throw new Error('Invalid token. Please check your Personal Access Token.');
        }
        throw new Error(`GitHub API error: ${response.status}`);
      }

      const data: GitHubRepo[] = await response.json();
      setRepos(data);
      setStep('select');
    } catch (err) {
      setError(err instanceof Error ? err.message : 'Failed to fetch repositories');
    } finally {
      setLoading(false);
    }
  };

  const handleTokenSubmit = (e: React.FormEvent) => {
    e.preventDefault();
    fetchRepos();
  };

  const waitForLibraryReady = async (maxAttempts = 30, delayMs = 2000) => {
    for (let attempt = 0; attempt < maxAttempts; attempt++) {
      setConnectingStatus(
        attempt === 0
          ? 'Initializing library...'
          : `Cloning repository... (${attempt + 1}/${maxAttempts})`
      );

      try {
        await getLibraryStatus();
        // Success!
        setConnectingStatus('Connected!');
        await new Promise((resolve) => setTimeout(resolve, 500));

        if (onConfigured) {
          onConfigured();
        } else {
          window.location.reload();
        }
        return;
      } catch {
        // Still initializing, wait and retry
        if (attempt < maxAttempts - 1) {
          await new Promise((resolve) => {
            retryRef.current = setTimeout(resolve, delayMs);
          });
        }
      }
    }

    // Failed after all attempts
    setError('Failed to initialize library. The repository may not be accessible.');
    setStep('select');
    setSaving(false);
    setCreating(false);
  };

  const handleRepoSelect = async (repo?: GitHubRepo) => {
    const repoToUse = repo ?? selectedRepo;
    if (!repoToUse) return;

    setSaving(true);
    setError(null);

    try {
      const current = readSavedSettings();
      // Use SSH URL for private repos, HTTPS for public
      const repoUrl = repoToUse.private ? repoToUse.ssh_url : repoToUse.clone_url;
      writeSavedSettings({ ...current, libraryRepo: repoUrl });

      // Show connecting step and wait for backend
      setConnectingRepoName(repoToUse.full_name);
      setStep('connecting');

      // Small delay to ensure settings are saved
      await new Promise((resolve) => setTimeout(resolve, 200));

      // Wait for library to be ready
      await waitForLibraryReady();
    } catch (err) {
      setError(err instanceof Error ? err.message : 'Failed to save');
      setSaving(false);
    }
  };

  const handleCreateRepo = async (e: React.FormEvent) => {
    e.preventDefault();

    const name = newRepoName.trim();
    if (!name) {
      setError('Please enter a repository name');
      return;
    }

    if (!/^[a-zA-Z0-9._-]+$/.test(name)) {
      setError('Repository name can only contain letters, numbers, hyphens, underscores, and dots');
      return;
    }

    setCreating(true);
    setError(null);

    try {
      const response = await fetch('https://api.github.com/user/repos', {
        method: 'POST',
        headers: {
          Authorization: `Bearer ${token.trim()}`,
          Accept: 'application/vnd.github+json',
          'Content-Type': 'application/json',
        },
        body: JSON.stringify({
          name,
          description: 'OpenAgent library configuration - MCPs, skills, and commands',
          private: newRepoPrivate,
          auto_init: true, // Initialize with README
        }),
      });

      if (!response.ok) {
        const errorData = await response.json().catch(() => ({}));
        if (response.status === 422 && errorData.errors?.[0]?.message?.includes('already exists')) {
          throw new Error('A repository with this name already exists');
        }
        throw new Error(errorData.message || `GitHub API error: ${response.status}`);
      }

      const newRepo: GitHubRepo = await response.json();

      // Directly connect the newly created repo
      await handleRepoSelect(newRepo);
    } catch (err) {
      setError(err instanceof Error ? err.message : 'Failed to create repository');
      setCreating(false);
    }
  };

  const filteredRepos = repos.filter((repo) =>
    repo.full_name.toLowerCase().includes(searchQuery.toLowerCase())
  );

  // Connecting step - waiting for backend to initialize library
  if (step === 'connecting') {
    return (
      <div className="flex items-center justify-center min-h-[calc(100vh-12rem)]">
        <div className="w-full max-w-md text-center">
          <div className="flex justify-center mb-6">
            <div className="flex h-16 w-16 items-center justify-center rounded-2xl bg-indigo-500/10 border border-indigo-500/20">
              {connectingStatus === 'Connected!' ? (
                <CheckCircle className="h-8 w-8 text-emerald-400" />
              ) : (
                <Loader className="h-8 w-8 text-indigo-400 animate-spin" />
              )}
            </div>
          </div>

          <h2 className="text-lg font-semibold text-white mb-2">
            {connectingStatus === 'Connected!' ? 'Connected!' : 'Connecting Repository'}
          </h2>
          <p className="text-sm text-white/50 mb-4">{connectingRepoName}</p>
          <p className="text-xs text-white/40">{connectingStatus}</p>
        </div>
      </div>
    );
  }

  // Create new repository step
  if (step === 'create') {
    return (
      <div className="flex items-center justify-center min-h-[calc(100vh-12rem)]">
        <div className="w-full max-w-md text-center">
          <button
            onClick={() => {
              setStep('select');
              setError(null);
            }}
            className="flex items-center gap-1 text-xs text-white/40 hover:text-white/60 transition-colors mb-4"
          >
            <ChevronLeft className="h-3 w-3" />
            Back
          </button>

          <div className="flex justify-center mb-6">
            <div className="flex h-16 w-16 items-center justify-center rounded-2xl bg-emerald-500/10 border border-emerald-500/20">
              <Plus className="h-8 w-8 text-emerald-400" />
            </div>
          </div>

          <h2 className="text-lg font-semibold text-white mb-2">Create New Repository</h2>
          <p className="text-sm text-white/50 mb-6">
            Create a new GitHub repository to store your library configuration.
          </p>

          <form onSubmit={handleCreateRepo} className="space-y-4">
            <div>
              <input
                type="text"
                value={newRepoName}
                onChange={(e) => {
                  setNewRepoName(e.target.value);
                  setError(null);
                }}
                placeholder="repository-name"
                className={cn(
                  'w-full rounded-xl border bg-white/[0.02] px-4 py-3 text-sm text-white placeholder-white/30 focus:outline-none transition-colors',
                  error
                    ? 'border-red-500/50 focus:border-red-500/50'
                    : 'border-white/[0.08] focus:border-indigo-500/50'
                )}
                disabled={creating}
              />
            </div>

            <div className="flex gap-2">
              <button
                type="button"
                onClick={() => setNewRepoPrivate(true)}
                className={cn(
                  'flex-1 flex items-center justify-center gap-2 rounded-xl border px-4 py-3 text-sm transition-colors',
                  newRepoPrivate
                    ? 'border-indigo-500/50 bg-indigo-500/10 text-white'
                    : 'border-white/[0.08] bg-white/[0.02] text-white/60 hover:bg-white/[0.04]'
                )}
              >
                <Lock className="h-4 w-4" />
                Private
              </button>
              <button
                type="button"
                onClick={() => setNewRepoPrivate(false)}
                className={cn(
                  'flex-1 flex items-center justify-center gap-2 rounded-xl border px-4 py-3 text-sm transition-colors',
                  !newRepoPrivate
                    ? 'border-indigo-500/50 bg-indigo-500/10 text-white'
                    : 'border-white/[0.08] bg-white/[0.02] text-white/60 hover:bg-white/[0.04]'
                )}
              >
                <Globe className="h-4 w-4" />
                Public
              </button>
            </div>

            {error && <p className="text-xs text-red-400 text-left">{error}</p>}

            <button
              type="submit"
              disabled={creating || !newRepoName.trim()}
              className="w-full flex items-center justify-center gap-2 rounded-xl bg-emerald-500 hover:bg-emerald-600 disabled:bg-emerald-500/50 px-4 py-3 text-sm font-medium text-white transition-colors disabled:cursor-not-allowed"
            >
              {creating ? (
                <>
                  <Loader className="h-4 w-4 animate-spin" />
                  Creating...
                </>
              ) : (
                <>
                  Create Repository
                  <ArrowRight className="h-4 w-4" />
                </>
              )}
            </button>
          </form>

          {showDetails && (
            <p className="mt-4 text-[11px] text-white/20">Details: {details}</p>
          )}
        </div>
      </div>
    );
  }

  // Select repository step
  if (step === 'select') {
    return (
      <div className="flex items-center justify-center min-h-[calc(100vh-12rem)]">
        <div className="w-full max-w-lg text-center">
          <button
            onClick={() => {
              setStep('token');
              setSelectedRepo(null);
              setSearchQuery('');
            }}
            className="flex items-center gap-1 text-xs text-white/40 hover:text-white/60 transition-colors mb-4"
          >
            <ChevronLeft className="h-3 w-3" />
            Back
          </button>

          <div className="flex justify-center mb-6">
            <div className="flex h-16 w-16 items-center justify-center rounded-2xl bg-indigo-500/10 border border-indigo-500/20">
              <GitBranch className="h-8 w-8 text-indigo-400" />
            </div>
          </div>

          <h2 className="text-lg font-semibold text-white mb-2">Select Repository</h2>
          <p className="text-sm text-white/50 mb-6">
            Choose a repository or create a new one for your library.
          </p>

          <div className="relative mb-4">
            <Search className="absolute left-3 top-1/2 h-4 w-4 -translate-y-1/2 text-white/30" />
            <input
              type="text"
              value={searchQuery}
              onChange={(e) => setSearchQuery(e.target.value)}
              placeholder="Search repositories..."
              className="w-full rounded-xl border border-white/[0.08] bg-white/[0.02] pl-10 pr-4 py-3 text-sm text-white placeholder-white/30 focus:outline-none focus:border-indigo-500/50 transition-colors"
            />
          </div>

          <div className="rounded-xl border border-white/[0.08] bg-white/[0.02] max-h-64 overflow-y-auto">
            {/* Create new repo option */}
            <button
              onClick={() => setStep('create')}
              className="w-full text-left px-4 py-3 border-b border-white/[0.04] transition-colors hover:bg-emerald-500/5 group"
            >
              <div className="flex items-center gap-2">
                <Plus className="h-4 w-4 text-emerald-400" />
                <span className="text-sm font-medium text-emerald-400 group-hover:text-emerald-300">
                  Create new repository
                </span>
              </div>
              <p className="text-xs text-white/40 mt-1 ml-6">
                Set up a new repository for your library
              </p>
            </button>

            {filteredRepos.length === 0 ? (
              <div className="p-4 text-sm text-white/40">
                {repos.length === 0 ? 'No repositories found' : 'No matching repositories'}
              </div>
            ) : (
              filteredRepos.map((repo) => (
                <button
                  key={repo.id}
                  onClick={() => setSelectedRepo(repo)}
                  className={cn(
                    'w-full text-left px-4 py-3 border-b border-white/[0.04] last:border-b-0 transition-colors',
                    selectedRepo?.id === repo.id ? 'bg-indigo-500/10' : 'hover:bg-white/[0.02]'
                  )}
                >
                  <div className="flex items-center justify-between">
                    <span className="text-sm font-medium text-white">{repo.full_name}</span>
                    {repo.private && (
                      <span className="text-[10px] px-1.5 py-0.5 rounded bg-amber-500/10 text-amber-400">
                        Private
                      </span>
                    )}
                  </div>
                  {repo.description && (
                    <p className="text-xs text-white/40 mt-1 truncate">{repo.description}</p>
                  )}
                </button>
              ))
            )}
          </div>

          {error && <p className="mt-3 text-xs text-red-400 text-left">{error}</p>}

          <button
            onClick={() => handleRepoSelect()}
            disabled={!selectedRepo || saving}
            className="w-full mt-4 flex items-center justify-center gap-2 rounded-xl bg-indigo-500 hover:bg-indigo-600 disabled:bg-indigo-500/50 px-4 py-3 text-sm font-medium text-white transition-colors disabled:cursor-not-allowed"
          >
            {saving ? (
              <>
                <Loader className="h-4 w-4 animate-spin" />
                Configuring...
              </>
            ) : (
              <>
                Connect Repository
                <ArrowRight className="h-4 w-4" />
              </>
            )}
          </button>

          {showDetails && (
            <p className="mt-4 text-[11px] text-white/20">Details: {details}</p>
          )}
        </div>
      </div>
    );
  }

  // Token entry step
  return (
    <div className="flex items-center justify-center min-h-[calc(100vh-12rem)]">
      <div className="w-full max-w-md text-center">
        <div className="flex justify-center mb-6">
          <div className="flex h-16 w-16 items-center justify-center rounded-2xl bg-indigo-500/10 border border-indigo-500/20">
            <Key className="h-8 w-8 text-indigo-400" />
          </div>
        </div>

        <h2 className="text-lg font-semibold text-white mb-2">Configure Library</h2>
        <p className="text-sm text-white/50 mb-6">
          Enter a GitHub Personal Access Token to browse and select a repository.
        </p>

        <form onSubmit={handleTokenSubmit} className="space-y-3">
          <div className="relative">
            <input
              type="password"
              value={token}
              onChange={(e) => {
                setToken(e.target.value);
                setError(null);
              }}
              placeholder="ghp_xxxxxxxxxxxxxxxxxxxx"
              className={cn(
                'w-full rounded-xl border bg-white/[0.02] px-4 py-3 text-sm text-white placeholder-white/30 focus:outline-none transition-colors',
                error
                  ? 'border-red-500/50 focus:border-red-500/50'
                  : 'border-white/[0.08] focus:border-indigo-500/50'
              )}
              disabled={loading}
            />
          </div>

          {error && <p className="text-xs text-red-400 text-left">{error}</p>}

          <button
            type="submit"
            disabled={loading || !token.trim()}
            className="w-full flex items-center justify-center gap-2 rounded-xl bg-indigo-500 hover:bg-indigo-600 disabled:bg-indigo-500/50 px-4 py-3 text-sm font-medium text-white transition-colors disabled:cursor-not-allowed"
          >
            {loading ? (
              <>
                <Loader className="h-4 w-4 animate-spin" />
                Loading repositories...
              </>
            ) : (
              <>
                Continue
                <ArrowRight className="h-4 w-4" />
              </>
            )}
          </button>
        </form>

        <p className="mt-6 text-xs text-white/30">
          Create a token with <code className="text-white/50">repo</code> scope.{' '}
          <a
            href="https://github.com/settings/tokens/new?scopes=repo&description=OpenAgent%20Library"
            target="_blank"
            rel="noopener noreferrer"
            className="inline-flex items-center gap-1 text-indigo-400/70 hover:text-indigo-400 transition-colors"
          >
            Create token
            <ExternalLink className="h-3 w-3" />
          </a>
        </p>

        {showDetails && (
          <p className="mt-4 text-[11px] text-white/20">Details: {details}</p>
        )}
      </div>
    </div>
  );
}
