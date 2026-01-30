'use client';

import Link from 'next/link';
import { GitBranch, AlertTriangle, ExternalLink, Settings } from 'lucide-react';

type LibraryUnavailableProps = {
  message?: string | null;
  onConfigured?: () => void;
};

export function LibraryUnavailable({ message }: LibraryUnavailableProps) {
  const details = message?.trim();
  const showDetails = !!details && details !== 'Library not initialized';

  return (
    <div className="flex items-center justify-center min-h-[calc(100vh-12rem)]">
      <div className="w-full max-w-lg text-center">
        <div className="flex justify-center mb-6">
          <div className="flex h-16 w-16 items-center justify-center rounded-2xl bg-amber-500/10 border border-amber-500/20">
            <AlertTriangle className="h-8 w-8 text-amber-400" />
          </div>
        </div>

        <h2 className="text-lg font-semibold text-white mb-2">Library Not Configured</h2>
        <p className="text-sm text-white/50 mb-6">
          The configuration library is not set up. Configure it to enable skills, commands, and templates.
        </p>

        <div className="rounded-xl border border-white/[0.08] bg-white/[0.02] p-5 text-left space-y-4">
          <div className="flex items-start gap-3">
            <div className="flex h-8 w-8 items-center justify-center rounded-lg bg-indigo-500/10 flex-shrink-0">
              <Settings className="h-4 w-4 text-indigo-400" />
            </div>
            <div>
              <h3 className="text-sm font-medium text-white mb-1">Configure Library Remote</h3>
              <p className="text-xs text-white/40 mb-2">
                Set the library remote URL in Settings to connect to your configuration repository.
              </p>
              <Link
                href="/settings"
                className="inline-flex items-center gap-1.5 text-xs text-indigo-400 hover:text-indigo-300 transition-colors"
              >
                Go to Settings
                <ExternalLink className="h-3 w-3" />
              </Link>
            </div>
          </div>

          <div className="flex items-start gap-3">
            <div className="flex h-8 w-8 items-center justify-center rounded-lg bg-indigo-500/10 flex-shrink-0">
              <GitBranch className="h-4 w-4 text-indigo-400" />
            </div>
            <div>
              <h3 className="text-sm font-medium text-white mb-1">Library Repository</h3>
              <p className="text-xs text-white/40 mb-2">
                Create a new repository using the library template:
              </p>
              <a
                href="https://github.com/Th0rgal/sandboxed-library-template"
                target="_blank"
                rel="noopener noreferrer"
                className="inline-flex items-center gap-1.5 text-xs text-indigo-400 hover:text-indigo-300 transition-colors"
              >
                sandboxed-library-template
                <ExternalLink className="h-3 w-3" />
              </a>
            </div>
          </div>
        </div>

        {showDetails && (
          <p className="mt-4 text-[11px] text-white/20">Details: {details}</p>
        )}
      </div>
    </div>
  );
}
