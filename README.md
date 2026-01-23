<p align="center">
  <img src="dashboard/public/favicon.svg" width="120" alt="Open Agent" />
</p>

<h1 align="center">Open Agent</h1>

<p align="center">
  <strong>Self-hosted cloud orchestrator for AI coding agents</strong><br/>
  Isolated Linux workspaces with Claude Code & OpenCode runtimes
</p>

<p align="center">
  <a href="#vision">Vision</a> ·
  <a href="#features">Features</a> ·
  <a href="#ecosystem">Ecosystem</a> ·
  <a href="#screenshots">Screenshots</a> ·
  <a href="#getting-started">Getting Started</a>
</p>

<br/>

<p align="center">
  <img src="screenshots/hero.webp" alt="Open Agent Dashboard" width="100%" />
</p>

<p align="center">
  <strong>Ready to deploy?</strong> Ask your local AI agent to check out <a href="./INSTALL.md">INSTALL.md</a> it'll guide you through everything.
</p>

---

## Vision

What if you could:

**Hand off entire dev cycles.** Point an agent at a GitHub issue, let it write code, test by launching a Minecraft server, and open a PR when tests pass. You review the diff, not the process.

**Run multi-day operations unattended.** Give an agent SSH access to your home GPU through a VPN. It reads Nvidia docs, sets up training, fine-tunes models while you sleep.

**Keep sensitive data local.** Analyze your sequenced DNA against scientific literature. Local inference, isolated containers, nothing leaves your machines.

---

## Features

- **Dual Runtime Support**: Run Claude Code or OpenCode agents in the same infrastructure
- **Mission Control**: Start, stop, and monitor agents remotely with real-time streaming
- **Isolated Workspaces**: Containerized Linux environments (systemd-nspawn) with per-mission directories
- **Git-backed Library**: Skills, tools, rules, agents, and MCPs versioned in a single repo
- **MCP Registry (optional)**: Extra tool servers (desktop/playwright/etc.) when needed
- **Multi-platform**: Web dashboard (Next.js) and iOS app (SwiftUI) with Picture-in-Picture

---

## Ecosystem

Open Agent orchestrates multiple AI coding agent runtimes:

- **[Claude Code](https://docs.anthropic.com/en/docs/claude-code)**: Anthropic's official coding agent with native skills support (`.claude/skills/`)
- **[OpenCode](https://github.com/anomalyco/opencode)**: Open-source alternative via [oh-my-opencode](https://github.com/code-yeongyu/oh-my-opencode)

Each runtime executes inside isolated workspaces, so bash commands and file operations are scoped correctly. Open Agent handles orchestration, workspace isolation, and Library-based configuration management.

---

## Screenshots

<p align="center">
  <img src="screenshots/dashboard-overview.webp" alt="Dashboard Overview" width="100%" />
</p>
<p align="center"><em>Real-time monitoring with CPU, memory, network graphs and mission timeline</em></p>

<br/>

<p align="center">
  <img src="screenshots/library-skills.webp" alt="Library Skills Editor" width="100%" />
</p>
<p align="center"><em>Git-backed Library with skills, commands, rules, and inline editing</em></p>

<br/>

<p align="center">
  <img src="screenshots/mcp-servers.webp" alt="MCP Servers" width="100%" />
</p>
<p align="center"><em>MCP server management with runtime status and Library integration</em></p>

---

## Getting Started

### Production Setup (the easy way)

1. Get a dedicated server (e.g., [Hetzner](https://www.hetzner.com/), Ubuntu 24.04)
2. Point a domain to your server IP
3. Clone this repo locally
4. Ask Claude to set it up:
   > "Please deploy Open Agent on my server at `<IP>` with domain `agent.example.com`"

That's it. Claude will handle nginx, SSL, systemd services, and everything else.

If you feel smarter than the AI, check out **[INSTALL.md](./INSTALL.md)**.

### Local Development

**Backend** (local dev):
```bash
export OPENCODE_BASE_URL="http://127.0.0.1:4096"
cargo run
```

`OPENCODE_BASE_URL` is optional for mission execution (per-workspace CLI is used). Keep it set if you rely on a shared OpenCode server for provider/auth management.

**Dashboard**:
```bash
cd dashboard
bun install
bun dev
```

Open `http://localhost:3001`

---

## Status

**Work in Progress** — This project is under active development. Contributions and feedback welcome.

## License

MIT
