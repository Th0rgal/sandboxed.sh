# Open Agent – Project Guide

Open Agent is a cloud orchestrator for AI coding agents, supporting both **Claude Code** and **OpenCode** runtimes. The backend **does not** run model inference or autonomous logic; it spawns per-mission CLI processes and focuses on orchestration, telemetry, and workspace/library management.

## Architecture Summary

- **Backend (Rust/Axum)**: mission orchestration, workspace/container management, MCP registry, Library sync.
- **Per-Mission Execution**: Each mission spawns its own CLI process (oh-my-opencode or claude) inside the workspace.
- **Dashboards**: `dashboard/` (Next.js) and `ios_dashboard/` (SwiftUI).

**No central server required**: Agents are read from the Library's `oh-my-opencode.json`, and missions execute via per-workspace CLI processes.

## Core Concepts

- **Library**: Git-backed config repo (skills, commands, agents, tools, rules, MCPs). `src/library/`. The default template is at [github.com/Th0rgal/openagent-library-template](https://github.com/Th0rgal/openagent-library-template).
- **Workspaces**: Host or container environments with their own skills, tools, and plugins. `src/workspace.rs` manages workspace lifecycle and syncs skills/tools to runtime-specific locations (`.opencode/` for OpenCode, `.claude/skills/` for Claude Code).
- **Missions**: Agent selection + workspace + conversation. Execution is delegated to the chosen runtime (Claude Code or OpenCode) and streamed to the UI.

## Scoping Model

- **Global**: Auth, providers, MCPs (run on HOST machine), agents, commands, rules
- **Per-Workspace**: Skills, tools, plugins/hooks, installed software (container only), file isolation
- **Per-Mission**: Agent selection, workspace selection, conversation history

MCPs can be global because and run as child processes on the host or workspace (run inside the container). It depends on the kind of MCP.

## Per-Mission CLI Execution (Important!)

Open Agent uses **per-mission CLI execution** for both OpenCode and Claude Code:

```
Mission Start
    ↓
WorkspaceExec.spawn_streaming()
    ↓
┌─────────────────────────────────────────────┐
│  Claude Code Runtime:                        │
│  claude --dangerously-skip-permissions ...  │
│                                              │
│  OpenCode Runtime:                           │
│  bunx oh-my-opencode run ...                │
│                                              │
│  Container: via systemd-nspawn              │
│  Host: direct execution                      │
└─────────────────────────────────────────────┘
    ↓
CLI process streams JSON events back
```

**Key points:**
- Each mission spawns its own CLI process inside the workspace
- For containers: CLI runs via `systemd-nspawn` inside the container filesystem
- For host: CLI runs directly on the host
- Built-in Bash executes inside the workspace (no MCP needed)
- Settings sync from Library → per-workspace config files

See `src/workspace_exec.rs` for the execution layer and `src/api/mission_runner.rs` for mission execution.

## Design Guardrails

- Do **not** reintroduce autonomous agent logic (budgeting, task splitting, verification, model selection). The runtime (Claude Code or OpenCode) handles execution.
- Keep the backend a thin orchestrator: **Start Mission → Stream Events → Store Logs**.
- Avoid embedding provider-specific logic in the backend. Provider auth is managed via runtime config + dashboard flows.

## Timeout Philosophy

Open Agent is a **pure pass-through frontend** to the runtime (Claude Code or OpenCode). We intentionally do NOT impose any timeouts on the event stream. All timeout handling is delegated to the runtime, which manages tool execution timeouts internally.

**Why no timeouts in Open Agent?**
- Long-running tools (vision analysis, large file operations, web scraping) should complete naturally
- Users can abort missions manually via the dashboard if needed
- Avoids artificial timeout mismatches between Open Agent and the runtime
- The runtime remains the single source of truth for execution limits

**What this means:**
- The event stream runs indefinitely until the runtime sends completion events or closes the connection
- The only timeout applied is for initial connection establishment
- If a mission appears stuck, check runtime logs first—any timeout errors originate from the runtime or downstream clients, not Open Agent

## Common Entry Points

- `src/api/routes.rs` – API routing and server startup.
- `src/api/control.rs` – mission control session, SSE streaming.
- `src/api/mission_runner.rs` – per-mission execution loop (handles both Claude Code and OpenCode).
- `src/workspace.rs` – workspace lifecycle + runtime config generation (skills synced to `.claude/skills/` or `.opencode/`).
- `src/opencode/` – OpenCode HTTP + SSE client.

## Local Dev

The backend must be deployed on a remote linux server and ran in debug mode (release is too slow).

```bash
# Backend (url is https://agent-backend.thomas.md but remote is 95.216.112.253)
# No OPENCODE_BASE_URL needed - per-mission CLI execution
cargo run --debug

# Dashboard
cd dashboard
bun install
bun dev
```

## Debugging Missions

Missions are persisted in a **SQLite database** with full event logging, enabling detailed post-mortem analysis.

**Database location**: `~/.openagent/missions/missions.db` (or `missions-dev.db` in dev mode)

**Retrieve events via API**:
```bash
GET /api/control/missions/{mission_id}/events
```

**Query parameters**:
- `types=<type1>,<type2>` – filter by event type
- `limit=<n>` – max events to return
- `offset=<n>` – pagination offset

**Event types captured**:
- `user_message` – user inputs
- `thinking` – agent reasoning tokens
- `tool_call` – tool invocations (name + input)
- `tool_result` – tool outputs
- `assistant_message` – agent responses
- `mission_status_changed` – status transitions
- `error` – execution errors

**Example**: Retrieve tool calls for a mission:
```bash
curl "http://localhost:3000/api/control/missions/<mission_id>/events?types=tool_call,tool_result" \
  -H "Authorization: Bearer <token>"
```

**Code entry points**: `src/api/mission_store/` handles persistence; `src/api/control.rs` exposes the events endpoint.

## Dashboard Data Fetching (SWR)

The dashboard uses [SWR](https://swr.vercel.app/) for data fetching with stale-while-revalidate caching. This provides instant UI updates from cache while revalidating in the background.

### Common SWR Keys

Use consistent keys to enable cache sharing across components:

| Key | Fetcher | Used In |
|-----|---------|---------|
| `'stats'` | `getStats` | Overview page (3s polling) |
| `'workspaces'` | `listWorkspaces` | Overview, Workspaces page |
| `'missions'` | `listMissions` | Recent tasks sidebar, History page |
| `'workspace-templates'` | `listWorkspaceTemplates` | Workspaces page |
| `'library-skills'` | `listLibrarySkills` | Workspaces page |
| `'ai-providers'` | `listAIProviders` | Settings page |
| `'health'` | `getHealth` | Settings page |
| `'system-components'` | `getSystemComponents` | Server connection card |
| `'opencode-agents'` | `getVisibleAgents` | New mission dialog |
| `'openagent-config'` | `getOpenAgentConfig` | New mission dialog |
| `'tools'` | `listTools` | Tools page |

### Usage Patterns

**Basic fetch (no polling):**
```tsx
const { data, isLoading, error } = useSWR('workspaces', listWorkspaces, {
  revalidateOnFocus: false,
});
```

**With polling:**
```tsx
const { data } = useSWR('stats', getStats, {
  refreshInterval: 3000,
  revalidateOnFocus: false,
});
```

**After mutations (revalidate cache):**
```tsx
const { mutate } = useSWR('missions', listMissions);
// After deleting a mission:
await deleteMission(id);
mutate(); // Revalidates from server
```

**Optimistic updates:**
```tsx
mutate(missions.filter(m => m.id !== deletedId), false); // Update cache without revalidation
```

### Guidelines

- Always use `revalidateOnFocus: false` unless you need tab-focus refresh
- Use the same SWR key when multiple components need the same data
- Prefer `mutate()` after mutations instead of manual state updates
- SWR returns `undefined` (not `null`) when data hasn't loaded - use `?? null` or `?? []` as needed

## Production Deployment

For deploying Open Agent on a VPS or dedicated server, see **[INSTALL.md](../INSTALL.md)**.

This covers:
- systemd service for Open Agent (no separate OpenCode service needed)
- nginx/Caddy reverse proxy with SSL (Let's Encrypt)
- DNS setup and domain configuration
- Authentication configuration
- Library git repo setup

**If asked to deploy Open Agent**: Read INSTALL.md first. It contains an "AI Agents" section at the top listing prerequisites to collect from the user (server IP, domain, SSH access, Library repo URL).

### Build Mode

**IMPORTANT: Always use debug builds unless release is explicitly requested.**

Debug builds are preferred because:
- Much faster compilation time
- Better error messages and stack traces
- Release builds are unnecessarily slow for this use case

When deploying, use `cargo build` (not `cargo build --release`).

## Notes

- OpenCode config files are generated per workspace; do not keep static `opencode.json` in the repo.
- Container workspaces require root and Ubuntu/Debian tooling (systemd-nspawn).
