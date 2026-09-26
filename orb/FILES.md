# Files panel

Read-only v1. Open Files from the conversation header or use Cmd/Ctrl+P.
A click previews a file; a double click pins its tab. The panel and explorer
resize independently. Cmd+/ switches Markdown preview/source when focus is
inside the panel; existing editors retain their shortcut. Closing the panel
preserves the conversation and draft. Tabs and tree state are scoped to the
backend and mission/controller in session storage.

References in completed assistant prose, inline code and Markdown links are
resolved in visible content, in batches. Unverified paths stay text. Multiple
matches open a chooser. Truncated inline paths offer filename search on right
click. Document links resolve relative to the current file, within its source.

Sources include the mission workspace, current project context, an attached
`.paloma` mission copy when present, the associated controller prompt, and
published file artifacts. Current context is explicitly distinguished from the
mission copy. Paths outside these sources are not exposed (including arbitrary
Hermes host files). Local missions use their persisted local workspace binding.

## Runtime support

- Core: authenticated `POST /api/file-resources` derives roots from the mission
  and project; clients cannot select an arbitrary server root.
- Node: authenticated `POST /jobs/:id/files` reads that job's mission directory.
- Local: Tauri `browse_local_files` uses the same shared filesystem engine.
- The shared engine rejects traversal, symlinks and credential paths, bounds
  scans and returns text/download chunks of at most 1 MiB.
- Old backends retain existing project-context browsing and display a backend
  update requirement for workspace browsing. Authentication/network errors do
  not trigger this compatibility path.

Rollout requires the new core and node binaries for remote browsing, and a
rebuilt Orb binary for local browsing. Frontend hot reload alone does not add a
Tauri command. No migration is required. This change does not deploy or restart
services automatically.

## Validation

`pnpm test`, `pnpm build`, and `pnpm test:browser tests/files.browser.spec.ts`
from `orb/`. The browser fixture covers dark/light layouts, narrow windows,
references, search, Markdown/source switching, and draft preservation.
`cargo test --manifest-path orb/src-tauri/Cargo.toml file_browser` checks
filesystem confinement. Linux core/node compilation is required because the
existing backend has Linux-specific filesystem calls.
