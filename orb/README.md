# Orb

Desktop client for sandboxed.sh (Tauri 2 + SolidJS): projects and their files,
running missions with live transcripts, machines (the remote-node fleet plus
Paloma SSH hosts), providers (CLIProxyAPI-owned OAuth vs API keys) and a New
Agent composer. Connect a backend in Settings → Backend (dashboard password);
the URL and JWT are kept in localStorage.

```
pnpm install
pnpm tauri dev      # run the app
pnpm tauri build    # release binary
```

Shortcuts: ⌘N new agent · ⌘B sidebar · ⌘[ / ⌘] history · ⌘, settings · Esc back · ⌘/ markdown source.

Start a draft with `/goal <objective>` to launch a goal-mode mission: the composer
shows a Goal tag, the objective becomes the title, and the backend receives the
canonical `/goal` prompt it already understands. Remote launches follow the
`remote_launch` capability advertised by `GET /api/remote-nodes`; harnesses the
server has not confirmed for a node are refused before any request is sent.

Cron forms share the same schedule, instruction, skills, repeat, advanced fields,
validation and discard behavior. Drafts are kept in session storage, scoped to
the backend and project/job, across navigation and dialog dismissal; Save or
Discard clears them. Browser close/reload also prompts while edits are pending.
Continuity means injecting the job's own previous output (`context_from: self`),
not creating or resuming a native session.

The Hermes REST whitelist at production revision `882881de` does not expose all
of its storage fields. `../patches/hermes/orb-cron-api-fields.patch` supplies that
API parity. Apply it as part of a separately managed Hermes release; Orb warns
when an older API drops an override. A successful creation with dropped overrides
still opens the created cron and reports the missing fields, avoiding a second
creation on retry. No patch is applied to a live service by the tests.

The single-package `pnpm-workspace.yaml` isolates Orb from parent workspaces and
allows only esbuild's install script. `packageManager` pins pnpm 10.17.1, whose
configuration uses `onlyBuiltDependencies`.

Local verification (no live backend required):

```sh
pnpm install --frozen-lockfile
pnpm build
pnpm test
pnpm exec playwright install chromium
pnpm test:browser
cargo +stable fmt --manifest-path src-tauri/Cargo.toml -- --check
```

Browser tests serve the actual components in a fixture harness and intercept all
API calls. They cover creation, cached root-folder refresh, revealing the created
cron, keyboard menus, tab/unmount draft retention, cursors, compact schedule layout,
and light/dark screenshots. The Hermes job records are captured from real storage
functions; see `tests/fixtures/README.md` for provenance and regeneration.

Local voice input (macOS on Apple Silicon): the composer's microphone records in
the webview and transcribes on-device with a pinned MLX build of Cohere
Transcribe, inserting editable text without sending. Install the isolated
runtime with `voice/install.sh --download-model --check`; architecture, paths,
verification commands and the remaining Mac checklist are in `voice/README.md`.
