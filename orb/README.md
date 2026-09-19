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
