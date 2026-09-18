# Orb

Desktop client for sandboxed.sh (Tauri 2 + SolidJS). The Agents-window UI is in
place: projects, machines, providers (shaped for CLIProxyAPI-owned OAuth vs
API keys), and a New Agent composer. **It is not wired to the API yet.**

```
pnpm install
pnpm tauri dev      # run the app
pnpm tauri build    # release binary
```

Shortcuts: ⌘N new agent · ⌘B sidebar · ⌘[ / ⌘] history · ⌘, settings · Esc back · ⌘/ markdown source.
