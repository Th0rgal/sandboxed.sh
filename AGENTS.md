# Open Agent Panel – Project Guide

Open Agent is a managed control plane for OpenCode-based agents. The backend **does not** run model inference or autonomous logic; it delegates execution to an OpenCode server and focuses on orchestration, telemetry, and workspace/library management.

## Architecture Summary

- **Backend (Rust/Axum)**: mission orchestration, workspace/chroot management, MCP registry, Library sync.
- **OpenCode Client**: `src/opencode/` and `src/agents/opencode.rs` (thin wrapper).
- **Dashboards**: `dashboard/` (Next.js) and `ios_dashboard/` (SwiftUI).

## Core Concepts

- **Library**: Git-backed config repo (skills, commands, agents, MCPs). `src/library/`.
- **Workspaces**: Host or chroot environments with their own skills and plugins. `src/workspace.rs` manages workspace lifecycle and syncs skills to `.opencode/skill/`.
- **Missions**: Agent selection + workspace + conversation. Execution is delegated to OpenCode and streamed to the UI.

## Scoping Model

- **Global**: Auth, providers, MCPs (run on HOST machine), agents, commands
- **Per-Workspace**: Skills, plugins/hooks, installed software (chroot only), file isolation
- **Per-Mission**: Agent selection, workspace selection, conversation history

MCPs are global because they run as child processes on the host, not inside chroots.
Skills and plugins are synced to workspace `.opencode/` directories.

## Design Guardrails

- Do **not** reintroduce autonomous agent logic (budgeting, task splitting, verification, model selection). OpenCode handles execution.
- Keep the backend a thin orchestrator: **Start Mission → Stream Events → Store Logs**.
- Avoid embedding provider-specific logic in the backend. Provider auth is managed via OpenCode config + dashboard flows.

## Common Entry Points

- `src/api/routes.rs` – API routing and server startup.
- `src/api/control.rs` – mission control session, SSE streaming.
- `src/api/mission_runner.rs` – per-mission execution loop.
- `src/workspace.rs` – workspace lifecycle + OpenCode config generation.
- `src/opencode/` – OpenCode HTTP + SSE client.

## Local Dev

```bash
# Backend
export OPENCODE_BASE_URL="http://127.0.0.1:4096"
cargo run --release

# Dashboard
cd dashboard
bun install
bun dev
```

## Notes

- OpenCode config files are generated per workspace; do not keep static `opencode.json` in the repo.
- Chroot workspaces require root and Ubuntu/Debian tooling.
