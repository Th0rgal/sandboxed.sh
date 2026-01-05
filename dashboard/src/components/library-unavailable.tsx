'use client';

type LibraryUnavailableProps = {
  message?: string | null;
};

export function LibraryUnavailable({ message }: LibraryUnavailableProps) {
  const details = message?.trim();
  const showDetails = !!details && details !== "Library not initialized";

  return (
    <div className="rounded-xl border border-white/[0.06] bg-white/[0.02] p-6">
      <div className="text-sm text-white/70">Library is not configured.</div>
      <p className="mt-2 text-xs text-white/40">
        Add a Git repo in <span className="text-white/70">Settings → Configuration Library</span>{' '}
        to enable MCPs, skills, and commands.
      </p>
      <p className="mt-3 text-xs text-white/40">
        Example:{' '}
        <code className="rounded bg-white/[0.06] px-1.5 py-0.5 text-[11px] text-white/70">
          https://github.com/your/library.git
        </code>
      </p>
      {showDetails ? (
        <p className="mt-3 text-[11px] text-white/30">Details: {details}</p>
      ) : null}
    </div>
  );
}
