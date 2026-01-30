# Workspaces

Workspaces are isolated execution environments where AI agents run missions.
Each workspace defines where commands execute, what tools are available, and
what secrets the agent can access.

## Concepts

### Workspace Types

**Host workspace** --- commands run directly on the server. The agent shares the
host filesystem and network. This is the default for quick tasks or when you
trust the agent with full system access.

**Container workspace** --- commands run inside an isolated Linux container
(systemd-nspawn). The agent gets its own filesystem, users, and optionally its
own network stack. Container workspaces are the recommended choice for
production missions: a misbehaving agent cannot damage the host.

### Templates

A **template** is a reusable blueprint for container workspaces. Templates are
stored in your Library repository under `workspace-template/<name>.json` and
define:

- **Distro** --- base Linux distribution (Ubuntu Noble, Jammy, Debian Bookworm,
  or Arch Linux).
- **Init script** --- a bash script that runs once when the container is first
  built. This is where you install packages, configure SSH keys, set up
  development tools, etc.
- **Skills** --- Library skills to sync into the workspace (e.g.,
  `github-cli`, `deployment-management`).
- **Environment variables** --- secrets and configuration injected at build time
  and available during missions.
- **Encrypted keys** --- env var names whose values are encrypted at rest.
- **Networking** --- shared (host network) or isolated (private network with
  optional Tailscale VPN).

When you create a workspace from a template, Sandboxed.sh:

1. Creates a minimal root filesystem using `debootstrap` (Debian/Ubuntu) or
   `pacstrap` (Arch).
2. Runs the init script inside the container.
3. Bootstraps agent harnesses (Claude Code, OpenCode, Amp CLIs).
4. Marks the workspace as `ready`.

Rebuilding a workspace destroys the container and re-runs the full process.
Re-running the init script (via the API) is faster for iterating on the script
without recreating the base filesystem.

### Missions and Workspaces

Each mission targets a specific workspace. When a mission starts, Sandboxed.sh:

1. Creates a per-mission working directory inside the workspace
   (`/workspaces/mission-<id>`).
2. Syncs skills, tools, and MCP server configs from the Library into that
   directory.
3. Launches the chosen AI harness (Claude Code, OpenCode, or Amp) inside the
   workspace.

The harness runs natively in the workspace context --- shell commands, file
operations, and git all execute inside the container (for container workspaces)
or on the host (for host workspaces).

## Networking

### Shared Network (default)

By default, container workspaces use the host's network stack
(`shared_network: true` or `null`). The container can reach the internet
directly. This is the simplest setup and works for most use cases.

### Isolated Network with Tailscale

For workspaces that need a residential IP address or VPN routing, set
`shared_network: false`. This gives the container a private virtual ethernet
interface (`host0`) with NAT via the host.

To route traffic through a home connection:

1. Run a **Tailscale exit node** on your home network:
   ```bash
   tailscale up --advertise-exit-node
   ```
   Approve it in the Tailscale admin console.

2. Set these **workspace environment variables**:
   - `TS_AUTHKEY` --- a Tailscale auth key for the workspace.
   - `TS_EXIT_NODE` --- the exit node's Tailscale IP (e.g., `100.116.71.62`).

3. Use the `tailscale-ubuntu` template (or add Tailscale to your own template).

The template's init script installs Tailscale and creates helper scripts:
- `sandboxed.sh-network-up` --- brings up the virtual ethernet and DHCP.
- `sandboxed.sh-tailscale-up` --- connects to your tailnet and sets the exit node.

**Host NAT requirement**: isolated networking needs IP forwarding and NAT rules
on the host. See the installation guide (section 8.3) for the `iptables`
setup.

### When to Use Each

| Scenario | Networking | Why |
|----------|-----------|-----|
| General coding tasks | Shared (default) | Simplest, full internet access |
| Web scraping / browsing | Isolated + Tailscale | Residential IP avoids bot detection |
| Security-sensitive work | Isolated (no Tailscale) | No outbound internet from container |
| Minecraft / game automation | Shared | Needs direct access to game servers |

## Built-in Tools

Every container workspace is provisioned with the standard development tooling
that Sandboxed.sh's MCP servers need:

- **Bun** (`/usr/local/bin/bun`, `/usr/local/bin/bunx`) --- JavaScript runtime
  used to spawn MCP servers (Playwright, etc). Symlinked to `/usr/local/bin/`
  so the MCP command resolver finds them.
- **uv** (`/root/.local/bin/uv`) --- fast Python package manager from
  [Astral](https://docs.astral.sh/uv/). Useful for installing Python tools and
  running scripts.
- **MCP tooling** --- `@playwright/mcp`, `@anthropic-ai/mcp`,
  `@anthropic-ai/mcp-cli` are pre-installed via `bun install --global` for
  Playwright browser automation.

The init script ensures these are installed and available in the container's
`PATH`.

## Template Reference

### Structure

Templates live in `workspace-template/<name>.json` in your Library repo:

```json
{
  "name": "my-template",
  "description": "A workspace for my project",
  "distro": "ubuntu-noble",
  "skills": ["github-cli", "deployment-management"],
  "env_vars": {
    "SSH_PRIVATE_KEY_B64": "<base64-encoded key>",
    "MY_API_KEY": "sk-..."
  },
  "encrypted_keys": ["SSH_PRIVATE_KEY_B64", "MY_API_KEY"],
  "init_script": "#!/bin/bash\nset -euo pipefail\napt-get update\napt-get install -y git curl\n",
  "shared_network": true
}
```

### Fields

| Field | Type | Description |
|-------|------|-------------|
| `name` | string | Template identifier |
| `description` | string | Human-readable description |
| `distro` | string | `ubuntu-noble`, `ubuntu-jammy`, `debian-bookworm`, or `arch-linux` |
| `skills` | string[] | Library skills to sync |
| `env_vars` | object | Environment variables available during init and missions |
| `encrypted_keys` | string[] | Env var names encrypted at rest (requires `PRIVATE_KEY`) |
| `init_script` | string | Bash script executed once at container build time |
| `shared_network` | bool/null | `true` or `null` = host network; `false` = isolated veth |

### Init Script Best Practices

- Start with `set -euo pipefail` and error trapping.
- Log to `/var/log/sandboxed.sh-init.log` for debugging.
- Use `retry()` wrappers for network operations (apt, curl) to handle transient
  failures.
- Guard installations with `if ! command -v <tool>` so re-running the init
  script is idempotent.
- Always install **bun** and create `/usr/local/bin/bun` +
  `/usr/local/bin/bunx` symlinks (required for MCP servers).
- Install **uv** for Python tooling.
- Clean up apt caches (`rm -rf /var/lib/apt/lists/*`) at the end to reduce
  container size.

### Included Templates

**ubuntu** --- General-purpose Ubuntu Noble workspace with SSH/GPG keys, GitHub
CLI, Bitwarden Secrets CLI, Bun, uv, and Playwright. Good starting point for
most tasks.

**tailscale-ubuntu** --- Same as `ubuntu` plus Tailscale VPN for residential IP
routing. Uses isolated networking (`shared_network: false`). Set `TS_AUTHKEY`
and `TS_EXIT_NODE` in env vars.

**minecraft** --- Specialized workspace for Minecraft development and
automation. Includes Java 21, Maven, Gradle, X11/i3 desktop stack, Shard
launcher, mc-cli, Playwright, and pre-configured Fabric + NeoForge profiles.

## Recommendations

**Start with the `ubuntu` template.** It includes everything most agents need:
git, SSH keys, GitHub CLI, Bun (for MCP servers), uv (for Python), and
Playwright (for browser automation). Customize by forking this template.

**Use container workspaces for production.** Host workspaces are convenient for
development but give the agent unrestricted access. Container workspaces isolate
the agent's filesystem and can be rebuilt cleanly.

**Keep secrets in encrypted env vars.** Add secret names to `encrypted_keys` and
set `PRIVATE_KEY` in the Sandboxed.sh environment. The values are encrypted at
rest in the Library repo and decrypted at mission runtime.

**Use `rerun-init` for fast iteration.** When developing a template's init
script, use `POST /api/workspaces/:id/rerun-init` instead of rebuilding the
entire container. This re-executes the init script on the existing filesystem.

**Pin tool versions in init scripts.** Use `--version` flags or download
specific release URLs rather than `@latest` for reproducible builds.

## API Quick Reference

See [WORKSPACE_API.md](WORKSPACE_API.md) for the full API reference. Key
endpoints:

| Action | Method | Endpoint |
|--------|--------|----------|
| List workspaces | GET | `/api/workspaces` |
| Create workspace | POST | `/api/workspaces` |
| Build container | POST | `/api/workspaces/:id/build` |
| Execute command | POST | `/api/workspaces/:id/exec` |
| Re-run init script | POST | `/api/workspaces/:id/rerun-init` |
| Get init log | GET | `/api/workspaces/:id/init-log` |
| Debug info | GET | `/api/workspaces/:id/debug` |
| Delete workspace | DELETE | `/api/workspaces/:id` |

Templates are managed through the Library API:

| Action | Method | Endpoint |
|--------|--------|----------|
| List templates | GET | `/api/library/workspace-template` |
| Get template | GET | `/api/library/workspace-template/:name` |
| Save template | PUT | `/api/library/workspace-template/:name` |
| Delete template | DELETE | `/api/library/workspace-template/:name` |
