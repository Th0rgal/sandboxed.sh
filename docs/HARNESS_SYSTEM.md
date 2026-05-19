# Harness System

Sandboxed.sh supports multiple execution backends ("harnesses") for running agent
missions. The current architecture is **per-workspace execution**: harness CLIs
run inside the selected workspace (host or container).

This document explains the harness architecture, configuration, and how to add
new backends.

## Overview

A **harness** (also called a backend) is an execution engine that runs agent
missions. Sandboxed.sh currently supports:

| Harness | Description | Configuration Model |
|---------|-------------|---------------------|
| **OpenCode** | OpenCode CLI executed inside each workspace | Per-workspace (`opencode.json`, `.opencode/`) |
| **Claude Code** | Claude CLI executed inside each workspace | Per-workspace (`CLAUDE.md`, `.claude/settings.local.json`) |
| **Codex** | Codex CLI/app-server driver executed inside each workspace | Per-workspace (`.codex/config.toml`, `.codex/skills/`) |
| **Gemini** | Gemini CLI executed inside each workspace | Per-workspace OpenCode-style MCP/tool config |
| **Grok Build** | Grok CLI executed inside each workspace | Per-workspace OpenCode-style MCP/tool config |

## Architecture (per-workspace)

```
┌─────────────────────────────────────────────────────────────────┐
│                         Mission Runner                          │
│                   (src/api/mission_runner.rs)                   │
└────────────────────────────┬────────────────────────────────────┘
                             │
                             ▼
┌─────────────────────────────────────────────────────────────────┐
│                     Workspace Execution Layer                   │
│                 (src/workspace_exec.rs)                         │
│  - host: spawn process directly                                 │
│  - container: systemd-nspawn                                    │
└──────────────┬───────────────────────────────┬──────────────────┘
               │                               │
               ▼                               ▼
┌──────────────────────────┐    ┌──────────────────────────────────┐
│     OpenCode CLI          │    │  Claude/Codex/Gemini/Grok CLIs │
│  (opencode or wrapper)    │    │  - native streaming protocols  │
│  - per-workspace config   │    │  - per-workspace config        │
└──────────────────────────┘    └──────────────────────────────────┘
```

### Key properties

- **Native bash works** because the harness runs inside the workspace.
- **No host proxy bash tools** are required for standard missions.
- **Per-workspace isolation** prevents cross-workspace file effects.

## Backend registry (metadata)

Sandboxed.sh still maintains a backend registry for:

- listing agents
- backend configuration UI
- provider/auth settings

Execution itself is handled by the mission runner via the workspace execution
layer, not by a centralized OpenCode server.

## OpenCode harness

OpenCode is executed **per workspace** using the CLI:

- Uses `oh-my-opencode run` to start an embedded OpenCode server.
- Reads config from `opencode.json` and `.opencode/opencode.json`.
- `oh-my-opencode.json` is synced into each workspace.
- Built-in `bash` is enabled; legacy `workspace_*` tools are disabled by default.

### Agents

OpenCode agents are defined in `oh-my-opencode.json`:

```json
{
  "agents": {
    "Sisyphus": {
      "model": "anthropic/claude-opus-4-5"
    },
    "document-writer": {
      "model": "google/gemini-3-flash-preview"
    }
  }
}
```

## Claude Code harness

Claude Code is executed **per workspace** using the CLI:

- `.claude/settings.local.json` defines MCP servers and tool permissions.
- `.claude/skills/<name>/SKILL.md` provides native skill support.
- `CLAUDE.md` provides per-workspace context.
- Built-in `Bash` is enabled in the permissions allowlist.

### OAuth credentials for long-running missions

For container workspaces using OAuth authentication, Sandboxed.sh writes Claude Code's
credentials file to enable automatic token refresh during long-running missions:

- **Container workspaces**: `/root/.claude/.credentials.json` inside the container
- **Host workspaces**: `$HOME/.claude/.credentials.json` on the host

This allows Claude Code to refresh expired access tokens automatically instead of
failing mid-mission. The credentials file includes the refresh token and expiry time.

## Codex harness

Codex is executed **per workspace** using the Codex CLI/app-server driver:

- `.codex/config.toml` defines MCP servers and Codex profile settings.
- `.codex/skills/<name>/SKILL.md` provides native skill support.
- OpenAI API keys and Codex/ChatGPT credentials are discovered by the backend
  and rotated when rate limits require it.
- Goal-mode missions keep the raw `/goal <objective>` prefix so the Codex
  driver can route through goal APIs instead of a plain turn.

## Gemini and Grok harnesses

Gemini and Grok Build run through their native CLIs inside the workspace:

- Gemini defaults to the configured Google/Gemini model when no override is
  supplied.
- Grok Build uses `GROK_CODE_XAI_API_KEY`, `XAI_API_KEY`, xAI provider entries,
  or the Grok CLI's own login cache.
- Both reuse the OpenCode-style workspace config generation for MCP/tool wiring.

### Harness bootstrap (auto-install)

For **container workspaces**, Sandboxed.sh can automatically install the required
CLIs during container build (best-effort):

- `SANDBOXED_SH_BOOTSTRAP_CLAUDECODE=true` (default)
- `SANDBOXED_SH_BOOTSTRAP_OPENCODE=true` (default)
- `SANDBOXED_SH_BOOTSTRAP_GROK=true` (default)

At runtime, harnesses can self-install on first use if missing:

- `SANDBOXED_SH_AUTO_INSTALL_CLAUDECODE=true` (default)
- `SANDBOXED_SH_AUTO_INSTALL_OPENCODE=true` (default)

OpenCode installation uses the official installer (`https://opencode.ai/install`)
and copies the binary to `/usr/local/bin/opencode`. This requires `curl` inside
the workspace. If `curl` is unavailable, the mission fails with a clear error
message instructing you to add it to the workspace template.

Claude Code and oh-my-opencode installation use `npm` in the workspace. If
`npm` is unavailable, the mission fails with a clear error message instructing you
to add Node/npm to the workspace template.

### CLI protocol (NDJSON)

Claude Code communicates via NDJSON streaming:

```bash
echo "prompt" | claude \
  --print \
  --output-format stream-json \
  --verbose \
  --include-partial-messages \
  --model "claude-sonnet-4-20250514" \
  --session-id "uuid"
```

Event types:
- `system` (init)
- `stream_event` (deltas)
- `assistant` (final content + tool calls)
- `user` (tool results)
- `result` (completion)

## Tool policy

Default per-workspace tool settings:

- **OpenCode**: built-in `bash` enabled; `workspace_*` disabled by default.
- **Claude Code**: built-in `Bash` enabled via permissions.
- **Codex/Gemini/Grok**: native CLI tools run in the selected workspace and use
  the generated MCP/tool configuration for that backend.

MCP tools (desktop/playwright/workspace) can be enabled when needed.

## MCP execution scope (current)

Workspace-scoped MCP servers (desktop/playwright/workspace) run **alongside the
harness process**:

- When the harness runs inside a container (per-workspace runner enabled), MCPs
  execute directly in that container.
- When the harness runs on the host (`SANDBOXED_SH_PER_WORKSPACE_RUNNER=false`),
  container workspaces wrap MCP commands with systemd-nspawn (when available) so
  tools still execute inside the container.

Desktop streaming note:
- The UI streams X11 from the **host** (Xvfb + MJPEG).
- Container-local X servers are not visible to the host unless `/tmp/.X11-unix`
  is bind-mounted and `DISPLAY` is set. Sandboxed.sh only does this for
  interactive shells, not for harness/MCP execution by default.

## Adding a new backend

To add a new backend (e.g., Codex):

1. Create a backend module under `src/backend/<backend>/`.
2. Register it in `src/api/routes.rs` for metadata/UI.
3. Implement a per-workspace execution path in the mission runner.
4. Update the dashboard to expose backend-specific settings.

## Mission runner integration

The mission runner selects the harness based on `backend_id` and spawns the CLI
inside the workspace execution context:

```rust
let result = match backend_id.as_str() {
    "opencode" => run_opencode_turn(...).await,
    "claudecode" => run_claudecode_turn(...).await,
    "codex" => run_codex_turn(...).await,
    "gemini" => run_gemini_turn(...).await,
    "grok" => run_grok_turn(...).await,
    _ => Err(anyhow!("Unknown backend")),
};
```
