# Open Agent

A minimal autonomous coding agent with full machine access, implemented in Rust.

## Features

- **HTTP API** for task submission and monitoring
- **OpenCode Integration** - Delegates execution to OpenCode server (uses your Claude Max subscription)
- **Workspace Isolation** - Directory-based or full chroot isolation
- **Mission System** - Background task execution with resumability and SSE streaming
- **Full Toolset** - File operations, terminal, web access, git, browser automation (optional)
- **iOS Dashboard** - Native iOS app with SwiftUI
- **Web Dashboard** - Next.js dashboard with real-time updates
- **Library System** - Git-based configuration management for Skills, Commands, and MCP servers
- **AI-maintainable** Rust codebase with strong typing

## Architecture

```
┌─────────────────┐     ┌─────────────────┐     ┌─────────────────┐
│  Web Dashboard  │────▶│  Open Agent API │────▶│  OpenCode       │
│  (Next.js)      │     │  (Rust/Axum)    │     │  Server         │
└─────────────────┘     └─────────────────┘     └────────┬────────┘
                                                          │
┌─────────────────┐                                       │
│  iOS Dashboard  │                                       ▼
│  (Swift/SwiftUI)│                            ┌─────────────────────┐
└────────┬────────┘                            │  Anthropic API      │
         │                                     │  (Claude Max)       │
         └────────────────────────────────────▶└─────────────────────┘

Workspaces:
  ┌─────────────────┐  ┌─────────────────┐
  │  Directory      │  │  Chroot         │
  │  (Simple)       │  │  (Full isolation)│
  └─────────────────┘  └─────────────────┘
```

## Quick Start

### Prerequisites

- Rust 1.70+ (install via [rustup](https://rustup.rs/))
- OpenCode server running locally or remotely
- Claude Max subscription (for Anthropic API access via OpenCode)

### Installation

```bash
git clone <repo-url>
cd santa-fe-v2
cargo build  # Use debug builds for development (faster compilation)
```

### Running the Backend

```bash
# Required: Point to OpenCode server
export OPENCODE_BASE_URL="http://127.0.0.1:4096"

# Optional: Choose OpenCode agent
export OPENCODE_AGENT="build"

# Optional: Auto-allow OpenCode permissions
export OPENCODE_PERMISSIVE="true"

# Optional: Configure model (uses OpenCode's configured model by default)
export DEFAULT_MODEL="claude-opus-4-5-20251101"

# Optional: Working directory (defaults to /root in production, . in dev)
export WORKING_DIR="."

# Start the backend
cargo run
```

The server starts on `http://127.0.0.1:3000` by default.

### Running the Web Dashboard

The dashboard uses **Bun** (not npm/yarn/pnpm) as the package manager.

```bash
cd dashboard

# Install dependencies
bun install

# Start dev server (port 3001)
bun dev

# Production build
bun run build
```

### Running the iOS Dashboard

```bash
cd ios_dashboard

# Generate Xcode project (if needed)
xcodegen generate

# Open in Xcode
open OpenAgentDashboard.xcodeproj

# Or build from command line
xcodebuild -scheme OpenAgentDashboard -destination "platform=iOS Simulator,name=iPhone 15 Pro" build
```

## Core Concepts

### Missions

Missions are background tasks executed by the agent with full autonomy.

- **Create**: Submit a mission via API or dashboard
- **Monitor**: Real-time progress via SSE streaming
- **Resume**: Missions can be paused and resumed
- **History**: Full execution history with events and artifacts

### Workspaces

Workspaces provide isolation for mission execution.

**Types**:
- **Directory**: Simple folder-based isolation (fast, lightweight)
- **Chroot**: Full Linux chroot environment with debootstrap (maximum isolation)

**Chroot Workspaces**:
```bash
# Create chroot workspace
curl -X POST http://localhost:3000/api/workspaces \
  -H "Content-Type: application/json" \
  -d '{"name":"my-chroot","workspace_type":"chroot"}'

# Build chroot (takes 5-10 minutes)
curl -X POST http://localhost:3000/api/workspaces/{id}/build
```

Chroot workspaces use debootstrap to create minimal Ubuntu/Debian root filesystems with full isolation.

### Library System

The library system manages reusable configurations stored in a git repository:

- **Skills**: Reusable agent capabilities with prompts and reference files
- **Commands**: Custom shell commands and scripts
- **MCP Servers**: Model Context Protocol server configurations

**Library Operations**:
```bash
# Get library status
curl http://localhost:3000/api/library/status

# Pull latest from remote
curl -X POST http://localhost:3000/api/library/sync

# Commit local changes
curl -X POST http://localhost:3000/api/library/commit \
  -H "Content-Type: application/json" \
  -d '{"message":"Add new skill"}'

# Push to remote
curl -X POST http://localhost:3000/api/library/push
```

### Control Session

Global interactive control session with SSE streaming for real-time interaction.

```bash
# Send message to agent
curl -X POST http://localhost:3000/api/control/message \
  -H "Content-Type: application/json" \
  -d '{"content":"Create a Python script that prints Hello World"}'

# Stream events (SSE)
curl http://localhost:3000/api/control/stream
```

## API Reference

### Mission Endpoints

| Method | Path | Description |
|--------|------|-------------|
| `GET` | `/api/control/missions` | List all missions |
| `POST` | `/api/control/missions` | Create new mission |
| `GET` | `/api/control/missions/{id}` | Get mission details |
| `POST` | `/api/control/missions/{id}/load` | Load mission into control session |
| `POST` | `/api/control/missions/{id}/cancel` | Cancel running mission |
| `POST` | `/api/control/missions/{id}/resume` | Resume paused mission |
| `DELETE` | `/api/control/missions/{id}` | Delete mission |

### Workspace Endpoints

| Method | Path | Description |
|--------|------|-------------|
| `GET` | `/api/workspaces` | List workspaces |
| `POST` | `/api/workspaces` | Create workspace |
| `GET` | `/api/workspaces/{id}` | Get workspace details |
| `POST` | `/api/workspaces/{id}/build` | Build chroot workspace |
| `DELETE` | `/api/workspaces/{id}` | Delete workspace |

### Library Endpoints

| Method | Path | Description |
|--------|------|-------------|
| `GET` | `/api/library/status` | Git status |
| `POST` | `/api/library/sync` | Pull from remote |
| `POST` | `/api/library/commit` | Commit changes |
| `POST` | `/api/library/push` | Push to remote |
| `GET` | `/api/library/skills` | List skills |
| `GET/PUT/DELETE` | `/api/library/skills/{name}` | Skill CRUD |
| `GET` | `/api/library/commands` | List commands |
| `GET/PUT/DELETE` | `/api/library/commands/{name}` | Command CRUD |
| `GET/PUT` | `/api/library/mcp` | MCP server config |

### File System (Remote Explorer)

| Method | Path | Description |
|--------|------|-------------|
| `GET` | `/api/fs/list` | List directory |
| `GET` | `/api/fs/download` | Download file |
| `POST` | `/api/fs/upload` | Upload file |
| `POST` | `/api/fs/mkdir` | Create directory |
| `POST` | `/api/fs/rm` | Remove file/directory |

### System

| Method | Path | Description |
|--------|------|-------------|
| `GET` | `/api/health` | Health check |
| `GET` | `/api/stats` | System statistics |
| `POST` | `/api/auth/login` | Authenticate (if auth enabled) |

## Available Tools

The agent has access to these tools:

| Tool | Description |
|------|-------------|
| `read_file` | Read file contents with optional line range |
| `write_file` | Write/create files |
| `delete_file` | Delete files |
| `list_directory` | List directory contents |
| `search_files` | Search for files by name pattern |
| `run_command` | Execute shell commands |
| `grep_search` | Search file contents with regex |
| `web_search` | Search the web |
| `fetch_url` | Fetch URL contents |
| `git_status` | Get git repository status |
| `git_diff` | Show git diff |
| `git_commit` | Create git commits |
| `git_log` | Show git log |

**Optional Tools** (require features enabled):
- **Browser automation**: Playwright integration for web automation
- **Desktop automation**: i3/Xvfb integration for GUI automation

## Configuration

### Environment Variables

| Variable | Default | Description |
|----------|---------|-------------|
| `OPENCODE_BASE_URL` | `http://127.0.0.1:4096` | OpenCode server URL |
| `OPENCODE_AGENT` | - | OpenCode agent name (build/plan/etc) |
| `OPENCODE_PERMISSIVE` | `true` | Auto-allow OpenCode permissions |
| `DEFAULT_MODEL` | `claude-sonnet-4-20250514` | Default LLM model |
| `WORKING_DIR` | `.` (dev) / `/root` (prod) | Working directory |
| `HOST` | `127.0.0.1` | Server bind address |
| `PORT` | `3000` | Server port |
| `MAX_ITERATIONS` | `50` | Max agent loop iterations |
| `DEV_MODE` | `false` | Bypass authentication (development only) |
| `DASHBOARD_PASSWORD` | - | Dashboard password (single-tenant) |
| `JWT_SECRET` | - | HMAC secret for JWT signing |
| `LIBRARY_PATH` | `{working_dir}/.openagent/library` | Library git repo path |
| `LIBRARY_REMOTE` | - | Git remote URL for library |
| `BROWSER_ENABLED` | `false` | Enable browser automation tools |
| `DESKTOP_ENABLED` | `false` | Enable desktop automation tools |
| `SUPABASE_URL` | - | Supabase project URL (for memory) |
| `SUPABASE_SERVICE_ROLE_KEY` | - | Supabase service role key |

### Authentication

**Development** (no auth):
```bash
export DEV_MODE=true
```

**Production** (single-tenant):
```bash
export DEV_MODE=false
export DASHBOARD_PASSWORD="your-password"
export JWT_SECRET="your-secret"
```

**Production** (multi-user):
```bash
export DEV_MODE=false
export JWT_SECRET="your-secret"
export OPEN_AGENT_USERS='[{"id":"user1","username":"alice","password":"..."}]'
```

## Development

### Backend (Rust)

```bash
# Build (debug mode - faster compilation)
cargo build

# Build for production (optimized)
cargo build --release

# Run with debug logging
RUST_LOG=debug cargo run

# Run tests
cargo test

# Format code
cargo fmt

# Check for issues
cargo clippy
```

**Important**: Always use debug builds (`cargo build`) for development. Only use `--release` for production deployment.

### Web Dashboard (Bun)

```bash
cd dashboard

# Install dependencies (ALWAYS use bun, not npm)
bun install

# Add package
bun add <package-name>

# Dev server
bun dev

# Production build
bun run build

# Run tests
bunx playwright test
```

**Test Coverage**:
- **Playwright**: 44 E2E tests (100% passing)
- **Coverage**: Navigation, Agents, Workspaces, Control, Settings, Overview, Library

### iOS Dashboard (Swift)

```bash
cd ios_dashboard

# Run tests
xcodebuild -scheme OpenAgentDashboardTests -destination "platform=iOS Simulator,name=iPhone 15 Pro" test
```

**Test Coverage**:
- **XCTest**: 23 unit tests (100% passing)
- **Coverage**: Models (13 tests), Theme (10 tests)

## Production Deployment

### Server Details

| Property | Value |
|----------|-------|
| Host | `95.216.112.253` |
| Backend URL | `https://agent-backend.thomas.md` |
| Dashboard URL | `https://agent.thomas.md` |
| Binary Path | `/usr/local/bin/open_agent` |
| Env File | `/etc/open_agent/open_agent.env` |
| Service | `systemctl status open_agent` |

### Deploy Script

```bash
# SSH to production server
ssh -i ~/.ssh/cursor root@95.216.112.253

# Pull latest code and rebuild
cd /root/open_agent
git pull
cargo build --release  # Use --release for production!

# Copy binaries
cp target/release/open_agent /usr/local/bin/
cp target/release/desktop-mcp /usr/local/bin/
cp target/release/host-mcp /usr/local/bin/

# Restart service
systemctl restart open_agent
```

## Design System - "Quiet Luxury + Liquid Glass"

The UI follows a cohesive design language:

- **Dark-first**: Dark mode is the default aesthetic
- **Deep charcoal**: No pure black, use `#121214`
- **Elevation**: Via color gradation, not drop shadows
- **Text**: `white/[opacity]` for hierarchy (e.g., `text-white/80`)
- **Accent**: Indigo-500 (`#6366F1`)
- **Borders**: Very subtle (`white/6` to `white/8`)
- **Motion**: `ease-out` transitions, no bounce

## Documentation

- `.claude/CLAUDE.md` - Full project instructions and architecture
- `MISSION_TESTS.md` - Test results and mission validation
- `ITERATION_8_COMPLETION.md` - Development completion status
- `IMPROVEMENTS_NEEDED.md` - Known issues and enhancement opportunities

## License

MIT
