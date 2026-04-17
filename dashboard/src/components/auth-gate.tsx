'use client';

import { useEffect, useMemo, useState } from 'react';
import { login, getHealth } from '@/lib/api';
import { clearJwt, getStoredUsername, getValidJwt, setJwt, setStoredUsername, signalAuthSuccess } from '@/lib/auth';
import { Lock } from 'lucide-react';

export function AuthGate({ children }: { children: React.ReactNode }) {
  const [ready, setReady] = useState(false);
  const [authRequired, setAuthRequired] = useState(false);
  const [isAuthed, setIsAuthed] = useState(true);
  const [authMode, setAuthMode] = useState<'disabled' | 'single_tenant' | 'multi_user'>('single_tenant');
  const [username, setUsername] = useState(getStoredUsername() ?? '');
  const [password, setPassword] = useState('');
  const [error, setError] = useState<string | null>(null);
  const [isSubmitting, setIsSubmitting] = useState(false);

  const needsLogin = useMemo(() => authRequired && !isAuthed, [authRequired, isAuthed]);

  useEffect(() => {
    let mounted = true;
    void (async () => {
      try {
        const health = await getHealth();
        if (!mounted) return;

        setAuthRequired(Boolean(health.auth_required));
        if (health.auth_mode) {
          setAuthMode(health.auth_mode);
        }
        if (!health.auth_required) {
          setIsAuthed(true);
        } else {
          setIsAuthed(Boolean(getValidJwt()));
        }
      } catch {
        if (mounted) {
          setAuthRequired(false);
          setIsAuthed(true);
        }
      } finally {
        if (mounted) setReady(true);
      }
    })();
    return () => {
      mounted = false;
    };
  }, []);

  useEffect(() => {
    const onAuthRequired = () => {
      clearJwt();
      setIsAuthed(false);
      setError(null);
    };
    window.addEventListener('openagent:auth:required', onAuthRequired);
    return () => window.removeEventListener('openagent:auth:required', onAuthRequired);
  }, []);

  // Re-validate authentication when API URL changes
  useEffect(() => {
    const onApiUrlChanged = async () => {
      // Clear existing auth state
      clearJwt();
      setReady(false);
      setError(null);

      try {
        const health = await getHealth();
        setAuthRequired(Boolean(health.auth_required));
        if (health.auth_mode) {
          setAuthMode(health.auth_mode);
        }
        if (!health.auth_required) {
          setIsAuthed(true);
        } else {
          setIsAuthed(false);
        }
      } catch {
        // On error (e.g., unreachable URL), allow access to avoid locking the user
        // This matches the initial mount behavior and lets users fix the URL in settings
        setAuthRequired(false);
        setIsAuthed(true);
      } finally {
        setReady(true);
      }
    };

    window.addEventListener('openagent:api:url-changed', onApiUrlChanged);
    return () => window.removeEventListener('openagent:api:url-changed', onApiUrlChanged);
  }, []);

  const onSubmit = async (e: React.FormEvent) => {
    e.preventDefault();
    if (authMode === 'multi_user' && !username.trim()) {
      setError('Username is required');
      return;
    }
    setIsSubmitting(true);
    setError(null);
    try {
      const res = await login(password, authMode === 'multi_user' ? username : undefined);
      setJwt(res.token, res.exp);
      if (authMode === 'multi_user') {
        setStoredUsername(username);
      }
      setIsAuthed(true);
      setPassword('');
      signalAuthSuccess();
    } catch (err) {
      if (err instanceof Error) {
        setError(err.message);
      } else {
        setError(authMode === 'multi_user' ? 'Invalid username or password' : 'Invalid password');
      }
    } finally {
      setIsSubmitting(false);
    }
  };

  if (!ready) {
    return (
      <div className="min-h-screen bg-[#121214] text-white" aria-label="Checking authentication">
        <div className="flex min-h-screen items-center justify-center">
          <div className="h-6 w-6 animate-spin rounded-full border-2 border-white/20 border-t-indigo-400" />
        </div>
      </div>
    );
  }

  if (!needsLogin) {
    return <>{children}</>;
  }

  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center animate-fade-in">
      <div className="absolute inset-0 bg-black/60 backdrop-blur-sm" />

      <div className="relative z-10 w-full max-w-md rounded-2xl glass-panel border border-white/[0.08] p-6 animate-slide-up">
        <div className="flex items-center gap-3 mb-4">
          <div className="flex h-10 w-10 items-center justify-center rounded-xl bg-indigo-500/10">
            <Lock className="h-5 w-5 text-indigo-400" />
          </div>
          <div>
            <h2 className="text-lg font-semibold text-white">Authenticate</h2>
            <p className="text-xs text-white/50">
              {authMode === 'multi_user'
                ? 'Sign in with your username and password'
                : 'Enter the dashboard password to continue'}
            </p>
          </div>
        </div>

        <form onSubmit={onSubmit} className="space-y-4">
          {authMode === 'multi_user' && (
            <div>
              <input
                type="text"
                value={username}
                onChange={(e) => setUsername(e.target.value)}
                placeholder="Username"
                autoCapitalize="none"
                autoCorrect="off"
                autoFocus={authMode === 'multi_user'}
                spellCheck={false}
                className="w-full rounded-lg border border-white/[0.06] bg-white/[0.02] px-4 py-3 text-sm text-white placeholder-white/30 focus:border-indigo-500/50 focus:outline-none transition-colors"
              />
            </div>
          )}
          <div>
            <input
              type="password"
              value={password}
              onChange={(e) => setPassword(e.target.value)}
              placeholder="Password"
              autoFocus={authMode !== 'multi_user'}
              className="w-full rounded-lg border border-white/[0.06] bg-white/[0.02] px-4 py-3 text-sm text-white placeholder-white/30 focus:border-indigo-500/50 focus:outline-none transition-colors"
            />
          </div>

          {error && (
            <div className="rounded-lg bg-red-500/10 border border-red-500/20 px-3 py-2">
              <p className="text-sm text-red-400">{error}</p>
            </div>
          )}

          <button
            type="submit"
            disabled={!password || (authMode === 'multi_user' && !username.trim()) || isSubmitting}
            className="w-full rounded-lg bg-indigo-500 hover:bg-indigo-600 px-4 py-3 text-sm font-medium text-white transition-colors disabled:opacity-50 disabled:cursor-not-allowed"
          >
            {isSubmitting ? 'Signing in…' : 'Sign in'}
          </button>
        </form>
      </div>
    </div>
  );
}
