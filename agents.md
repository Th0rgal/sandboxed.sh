# Agents and Execution Architecture

> **⚠️ Debugging Issues?** Before investigating any runtime problems, **always
> read [DEBUGGING.md](DEBUGGING.md) first**. It contains:
> - Remote server SSH access (Thomas/Ben servers)
> - Systemd service management commands
> - Log viewing and common troubleshooting steps
> - Deployment procedures
>
> The dashboard typically runs locally but connects to **remote backends**. Debug
> on the server, not locally.

> **⚠️ IMPORTANT: Format Check Before Pushing**
>
> **ALWAYS run `cargo fmt --all` before committing Rust code changes!**
>
> The CI pipeline will fail if code is not properly formatted. To check locally:
> ```bash
> cargo fmt --all --check  # Check if formatting is needed
> cargo fmt --all          # Apply formatting
> ```
>
> Make this part of your pre-commit routine to avoid CI failures.

This document describes how Sandboxed.sh executes missions after the per-workspace
harness refactor. The core change: **agent harnesses run inside
the target workspace**, so native bash and file effects are scoped to the correct
environment. The host proxy bash tools are no longer required for normal
missions.

## High-level flow

1. User creates a mission with a workspace + agent (backend).
2. Sandboxed.sh prepares a **per-mission workspace directory** and syncs Library
   content (skills/tools/rules).
3. Sandboxed.sh writes **per-workspace config files** (`opencode.json`,
   `.opencode/opencode.json`, `.claude/settings.local.json`, `CLAUDE.md`).
4. The mission runner launches the chosen harness **inside the workspace** using
   a workspace-aware execution layer (host or container).
5. The harness streams JSON events; Sandboxed.sh converts these into a unified
   event stream for the UI.

## Execution model (per-workspace)

Sandboxed.sh uses a workspace execution layer to spawn processes in the correct
execution context:

- **Host workspace**: process runs directly on the host with the mission working
  directory as `cwd`.
- **Container workspace**: process runs inside the container via
  `systemd-nspawn`. This guarantees that built-in bash (OpenCode `bash` / Claude
  Code `Bash`) executes **inside the workspace**. File creation, git operations,
  and shell commands land in the correct workspace without a host-proxy tool.

## Harnesses

### OpenCode

- Runs **per workspace** using the `opencode run` CLI.
- Reads configuration from:
  - `opencode.json` at the workspace root
  - `.opencode/opencode.json`
- Built-in bash is **enabled** in per-workspace configs.

### Claude Code

- Runs **per workspace** using the Claude CLI.
- Configuration is written to each workspace:
  - `.claude/settings.local.json` (MCP servers + permissions)
  - `.claude/skills/<name>/SKILL.md` (native skills with YAML frontmatter)
  - `CLAUDE.md` (general workspace context)
- For OAuth auth, credentials are written to `$HOME/.claude/.credentials.json`
  (or `/root/.claude/.credentials.json` in containers) to enable token refresh.
- Built-in `Bash` is **enabled** in the permissions allowlist.

### Codex

- Runs **per workspace** using the Codex CLI/app-server driver.
- Configuration is written to each workspace:
  - `.codex/config.toml` (MCP servers and profile config)
  - `.codex/skills/<name>/SKILL.md` (native skills with YAML frontmatter)
- Auth uses OpenAI API keys or Codex/ChatGPT credentials discovered by the
  backend.

### Gemini and Grok

- Run **per workspace** using their native CLI backends.
- Reuse the OpenCode-style workspace config path for MCP/tool wiring.
- Auth is provider-specific: Gemini uses Google credentials/API keys, while Grok
  uses xAI API keys or the Grok CLI's own login cache.

## Tool policy

- **Built-in bash is the default** for OpenCode and Claude Code.
- Legacy MCP tool namespaces (`workspace_*`, `desktop_*`) are **disabled by
  default** in per-workspace OpenCode configs.
- Desktop/Playwright tools remain available as optional MCPs when needed.

If a mission truly requires MCP tools, re-enable them per workspace or per
backend in configuration. The default is to avoid host-proxy tooling.

## Desktop streaming (X11)

- The desktop stream is hosted on the **Sandboxed.sh host** (Xvfb + MJPEG).
- Container workspaces do **not** see the host desktop by default because the
  X11 socket (`/tmp/.X11-unix`) is not bind-mounted for harness/MCP execution.
- Interactive shells bind X11 when a runtime display is present, but harnesses
  and MCPs do not. If you need container agents to drive the shared desktop, add
  an explicit X11 bind + `DISPLAY`, or run the mission on a host workspace.

## Configuration sources

Per-workspace config is generated from three sources:

1. **Library** (git-backed) for agents, skills, tools, rules, and MCP
   definitions.
2. **Backend Settings** (UI) for CLI paths or backend-specific overrides.
3. **Workspace Settings** for env vars and per-workspace overrides.

Files written per mission workspace:

- `opencode.json` and `.opencode/opencode.json`
- `.claude/settings.local.json` (for Claude Code)
- `.claude/skills/<name>/SKILL.md` (native Claude Code skills)
- `CLAUDE.md` (general workspace context)
- `.codex/config.toml` and `.codex/skills/<name>/SKILL.md` (for Codex)

## Observability

Sandboxed.sh streams structured tool events and text deltas from the harnesses.
The UI receives:

- tool calls/results
- thinking deltas
- final completion

This preserves the UI experience while keeping execution isolated per workspace.

## Resource isolation (cgroups: memory + CPU)

Mission work is CPU/RAM-heavy and bursty (a single mission can spawn dozens of
parallel Lean/proof or contract builds). To keep one mission from starving the
host, the API service, or even its own harness's response streaming, every
mission container/exec runs in a **transient systemd scope** under
`missions.slice` — a cgroup *sibling* of the API service (`system.slice`), not a
child. Two tiers of caps apply:

- **Aggregate (slice level, ops-owned).** `missions.slice` carries a drop-in
  with a `CPUWeight` **lower** than `system.slice` (so missions yield CPU to the
  API tier under contention) plus an aggregate `MemoryHigh`/`MemoryMax` and an
  optional `CPUQuota` ceiling. This is the guarantee that the API → SSE → UI
  path stays responsive no matter how hot the missions get. The API service
  itself runs at a high `CPUWeight` within `system.slice`.
- **Per-mission (scope level, code-owned).** `MissionResourceCaps`
  (`workspace_exec.rs`) emits `MemoryMax/High/SwapMax` and `CPUWeight/CPUQuota`
  on each scope via `systemd-run --property`. Sourced from env with a
  per-workspace override: `MISSION_MEMORY_{MAX,HIGH,SWAP_MAX}` and
  `MISSION_CPU_{WEIGHT,QUOTA}` (see `.env.example`). The Resources panel
  (`POST /api/workspaces/:id/resources`) retunes running scopes live via
  `systemctl set-property --runtime` and persists the override.

Gotchas:
- **The harness CLI and the build it spawns share one scope** (the Bash tool's
  child), so cgroups can't separate harness from build *within* a mission — the
  per-mission `CPUQuota` (capping the mission) and the slice tiering (capping
  missions vs the API) are the real levers, not per-process priority.
- **Emitting a CPU property is what delegates the `cpu` controller into
  `missions.slice`.** Without any CPU property, `missions.slice`
  `cgroup.subtree_control` lacks `cpu` and a per-scope `CPUQuota` is silently
  unenforced. Only memory caps had been shipped, so on prod every slice sat at
  the default `cpu.weight=100` and Lean fan-outs oversubscribed all cores.
- Unset env (default) = no scope wrapping at all, so Docker installs without
  systemd PID 1 stay on the direct path.

## Resource isolation (container `/tmp`)

systemd-nspawn mounts a **tmpfs on `/tmp` inside every container**, sized at 10%
of host RAM (6.3G on a 64G box). Mission scratch — multi-GB audit trees, cert
runs, Ask sandbox copies — therefore lands in *RAM*, priced against the same
budget as the caps above. Setting `SANDBOXED_SH_CONTAINER_TMP_ROOT` backs each
container's `/tmp` with a per-workspace directory on a data disk instead
(`container_tmp.rs`, bound in `start_persistent_container_leader`). Retention for
stale scratch is `SANDBOXED_SH_CONTAINER_TMP_RETENTION_HOURS` (default 72); the
sweep rides the mission-workspace GC cadence.

Gotchas:
- **A full container `/tmp` is silent.** Writes get ENOSPC, but tooling that
  doesn't check leaves 0-byte logs and empty directories — no crash, no alert.
  In the 2026-08-05 incident two containers sat at zero bytes free for ~2 days.
- **It is invisible from the host.** Host-side `containers/<ws>/tmp` is an empty
  shadow of the tmpfs, so `df`, the disk watcher, and the workspace GC all
  report healthy numbers. To inspect it you must enter the namespace via the
  nspawn **child** pid (the nspawn pid itself still shows the host fs):
  `p=$(pgrep -f "systemd-nspawn .*--machine=sandboxed-<ws>-"|head -1); c=$(pgrep -P $p|head -1); nsenter -t $c -m -- df -h /tmp`
- **Raising the tmpfs size is not the fix.** Eleven containers at the default
  already oversubscribe a 62G box; disk-backing it is what buys real margin.
- Takes effect only when a container leader next starts (execs `nsenter` into
  the existing leader), so rollout is per-container as they cycle.
- Unset (default) = stock nspawn tmpfs, so shipping this changes nothing until
  the env is set.

## Operational notes

- **No central OpenCode server needed**: Missions spawn per-workspace CLI
  processes.
- **Compute placement is observable**: Hermes/controllers should call
  `get_compute_fleet` before parallel dispatch. Auto placement preserves idle
  GPU nodes for inference while ordinary CPU/Lean nodes have immediate slots,
  then balances by normalized utilization. Only terminal node/job/head receipts
  prove that work was actually distributed.
- **Dev/prod companion binaries are isolated**: production uses unsuffixed MCP
  and palomactl paths; non-production services use a service suffix (for
  example `assistant-mcp-dev`). A dev deploy must never replace Hermes's
  production connector.
- Agents are loaded from OpenCode built-ins and native `.opencode/agents/*.md` files.
- Per-workspace execution eliminates host-to-container network issues.
- For remote workspaces, SSH execution keeps bash/tooling on the remote host.

## Quick validation

Recommended smoke tests after changes:

1. **Claude Code (isolated)**: create a file and verify it exists inside the
   container workspace directory.
2. **OpenCode (isolated)**: create a file and verify it exists inside the
   container workspace directory.
3. **Codex/Gemini/Grok (isolated)**: create a file and verify it exists inside
   the container workspace directory.
4. **Claude Code (host)**: create a file in the host workspace.
5. **OpenCode (host)**: create a file in the host workspace.

If files appear in the wrong place, the harness is not running inside the
workspace execution context.

## Debugging Deployed Instances

For debugging production deployments, SSH access, systemd service management,
and log analysis, see **[DEBUGGING.md](DEBUGGING.md)**.
