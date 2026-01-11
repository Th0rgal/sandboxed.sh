# Open Agent

A managed control plane for OpenCode-based agents. Install it on your server to run agents in isolated workspaces and keep all configs synced through a Git-backed Library.

## What it does

- **Mission control**: start, stop, and monitor agents on a remote machine.
- **Workspace isolation**: host or container workspaces (systemd-nspawn) with per-mission directories.
- **Library sync**: Git-backed configs for skills, commands, agents, tools, rules, and MCPs.
- **Provider management**: manage OpenCode auth/providers from the dashboard.

## Architecture

1. **Backend (Rust/Axum)**
   - Manages workspaces + container lifecycle (systemd-nspawn).
   - Syncs skills, tools, and plugins to workspace `.opencode/` directories.
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

- **Library**: Git repo containing agent configs (skills, commands, agents, tools, rules, MCPs). The default template is at [github.com/Th0rgal/openagent-library-template](https://github.com/Th0rgal/openagent-library-template).
- **Workspaces**: Execution environments (host or container) with their own skills, tools, and plugins. Skills are synced to `.opencode/skill/` and tools to `.opencode/tool/` for OpenCode to discover.
- **Agents**: Library-defined capabilities (model, permissions, rules). Selected per-mission.
- **Missions**: Agent selection + workspace + conversation.
- **MCPs**: Global MCP servers run on the host machine (not inside containers).

## oh-my-opencode integration

Open Agent can sync Library plugins into OpenCode’s global config so agent packs like `oh-my-opencode` stay installed.

- If `oh-my-opencode` is installed, **Sisyphus** becomes the OpenCode default agent. Avoid overriding the agent in Open Agent unless you intentionally want a different one.
- You can still pick a different agent per mission in the dashboard; the selected agent is passed directly to OpenCode.
- Plugin syncing is managed by Open Agent whenever the Library is synced or plugins are saved.

## Quick start

### Prerequisites
- Rust 1.75+
- Bun 1.0+ (dashboard)
- An OpenCode server reachable from the backend
- Ubuntu/Debian recommended if you need container workspaces (systemd-nspawn)

### Backend
```bash
# Required: OpenCode endpoint
export OPENCODE_BASE_URL="http://127.0.0.1:4096"

# Optional defaults
export WORKING_DIR="/root"
export LIBRARY_REMOTE="git@github.com:Th0rgal/openagent-library-template.git"

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
