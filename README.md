# Open Agent Panel

A managed control panel for OpenCode-based agents. Install it on your server to run missions in isolated workspaces, stream live telemetry to the dashboards, and keep all agent configs synced through a Git-backed Library.

## What it does

- **Mission control**: start, stop, and monitor missions on a remote machine.
- **Workspace isolation**: host or chroot workspaces with per-mission directories.
- **Library sync**: Git-backed configs for skills, commands, agents, and MCPs.
- **Provider management**: manage OpenCode auth/providers from the dashboard.
- **Live telemetry**: stream thinking/tool events to web and iOS clients.

## Architecture

1. **Backend (Rust/Axum)**
   - Manages workspaces + chroot lifecycle.
   - Writes OpenCode workspace config (per-mission `opencode.json`).
   - Delegates execution to an OpenCode server and streams events.
   - Syncs the Library repo.

2. **Web dashboard (Next.js)**
   - Mission timeline, logs, and controls.
   - Library editor and MCP management.
   - Workspace and agent configuration.

3. **iOS dashboard (SwiftUI)**
   - Mission monitoring on the go.
   - Picture-in-Picture for desktop automation.

## Key concepts

- **Library**: Git repo containing agent configs (skills, commands, MCPs, tools).
- **Workspaces**: Execution environments (host or chroot) with their own paths.
- **Agents**: Library-defined capabilities and MCP subsets.
- **Missions**: A specific agent + workspace + prompt with streaming telemetry.

## Quick start

### Prerequisites
- Rust 1.75+
- Bun 1.0+ (dashboard)
- An OpenCode server reachable from the backend
- Ubuntu/Debian recommended if you need chroot workspaces

### Backend
```bash
# Required: OpenCode endpoint
export OPENCODE_BASE_URL="http://127.0.0.1:4096"

# Optional defaults
export DEFAULT_MODEL="claude-opus-4-5-20251101"
export WORKING_DIR="/root"
export LIBRARY_REMOTE="git@github.com:your-org/agent-library.git"

cargo run --release
```

### Web dashboard
```bash
cd dashboard
bun install
bun dev
```
Open `http://localhost:3001`.

### iOS app
Open `ios_dashboard` in Xcode and run on a device or simulator.

## Repository layout

- `src/` — Rust backend
- `dashboard/` — Next.js web app
- `ios_dashboard/` — SwiftUI iOS app
- `docs/` — ops + setup docs

## License
MIT
