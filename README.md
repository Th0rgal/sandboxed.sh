<p align="center">
  <img src="dashboard/public/favicon.svg" width="120" alt="Open Agent" />
</p>

<h1 align="center">Open Agent</h1>

<p align="center">
  <strong>Self-hosted control plane for AI autonomous agents</strong><br/>
  Isolated Linux workspaces and git-backed Library configuration
</p>

<p align="center">
  <a href="#vision">Vision</a> ·
  <a href="#features">Features</a> ·
  <a href="#ecosystem">Ecosystem</a> ·
  <a href="#screenshots">Screenshots</a> ·
  <a href="#getting-started">Getting Started</a>
</p>

---

## Vision

Open Agent is built for a future where AI agents handle complex, long-running tasks autonomously. Here's what I want to be able to achieve:

### 1. Cloud-native development
An agent that reads GitHub issues, writes code, launches a Minecraft server to test changes, and opens pull requests when everything works. Full development cycle, zero manual intervention.

### 2. Long-running autonomous tasks
Give an agent VPN access to my home network and let it SSH into my machines. I want it to be able to read Nvidia documentation and orchestrate multiple days tasks like LLM fine-tunning.

### 3. Private data on self-hosted infrastructure
I should be able to run everything locally (including LLM inference) and use isolated environments so sensitive data never leaves my machines. I want to be able to ask my agent to analyze my sequenced DNA, to cross-reference each allele against scientific literature without leaking anything about me.

---

## Features

- **Mission Control**: Start, stop, and monitor agents remotely with real-time streaming
- **Isolated Workspaces**: Containerized Linux environments (systemd-nspawn) with per-mission directories
- **Git-backed Library**: Skills, tools, rules, agents, and MCPs versioned in a single repo
- **MCP Registry**: Global MCP servers running on the host, available to all workspaces
- **Multi-platform**: Web dashboard (Next.js) and iOS app (SwiftUI) with Picture-in-Picture

---

## Ecosystem

Open Agent is a control plane for [**OpenCode**](https://github.com/anomalyco/opencode), the open-source AI coding agent. It delegates all model inference and autonomous execution to OpenCode while handling orchestration, workspace isolation, and configuration management.

Works great with [**oh-my-opencode**](https://github.com/code-yeongyu/oh-my-opencode) for enhanced agent capabilities and prebuilt skill packs.

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

## Architecture

```
┌─────────────────────────────────────────────────────────────────┐
│                         Open Agent                              │
├─────────────────────────────────────────────────────────────────┤
│  Dashboard (Next.js)  │  iOS App (SwiftUI)                      │
├───────────────────────┴─────────────────────────────────────────┤
│                     Rust Backend (Axum)                         │
│  ┌─────────────┐  ┌─────────────┐  ┌─────────────┐              │
│  │  Missions   │  │ Workspaces  │  │   Library   │              │
│  │  (Control)  │  │ (Isolation) │  │ (Git Sync)  │              │
│  └─────────────┘  └─────────────┘  └─────────────┘              │
├─────────────────────────────────────────────────────────────────┤
│                     OpenCode Server                             │
│              (Model inference & execution)                      │
└─────────────────────────────────────────────────────────────────┘
```

---

## Getting Started

### Prerequisites
- Rust 1.75+
- Bun 1.0+
- [OpenCode](https://github.com/anomalyco/opencode) server
- Linux host (Ubuntu/Debian for container workspaces)

### Backend
```bash
export OPENCODE_BASE_URL="http://127.0.0.1:4096"
cargo run --release
```

### Dashboard
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
