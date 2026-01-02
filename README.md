# Open Agent

A minimal autonomous coding agent with full machine access, implemented in Rust.

## Features

- **HTTP API** for task submission and monitoring
- **Tool-based agent loop** following the "tools in a loop" pattern
- **Full toolset**: file operations, terminal, machine-wide search, web access, git
- **OpenRouter integration** for LLM access (supports any model)
- **SSE streaming** for real-time task progress
- **AI-maintainable** Rust codebase with strong typing

## Quick Start

### Prerequisites

- Rust 1.70+ (install via [rustup](https://rustup.rs/))
- An OpenRouter API key ([get one here](https://openrouter.ai/))

### Installation

```bash
git clone <repo-url>
cd open_agent
cargo build --release
```

### Running

```bash
# Set your API key
export OPENROUTER_API_KEY="sk-or-v1-..."

# Optional: configure model (default: anthropic/claude-sonnet-4.5)
export DEFAULT_MODEL="anthropic/claude-sonnet-4.5"

# Optional: default working directory for relative paths (absolute paths work everywhere)
# In production this is typically /root
export WORKING_DIR="."

# Start the server
cargo run --release
```

The server starts on `http://127.0.0.1:3000` by default.

### OpenCode Backend (External Agent)

Open Agent can delegate execution to an OpenCode server instead of using its built-in agent loop.

```bash
# Point to a running OpenCode server
export AGENT_BACKEND="opencode"
export OPENCODE_BASE_URL="http://127.0.0.1:4096"

# Optional: choose OpenCode agent (build/plan/etc)
export OPENCODE_AGENT="build"

# Optional: auto-allow all permissions for OpenCode sessions (default: true)
export OPENCODE_PERMISSIVE="true"
```

## API Reference

### Submit a Task

```bash
curl -X POST http://localhost:3000/api/task \
  -H "Content-Type: application/json" \
  -d '{"task": "Create a Python script that prints Hello World"}'
```

Response:
```json
{
  "id": "550e8400-e29b-41d4-a716-446655440000",
  "status": "pending"
}
```

### Get Task Status

```bash
curl http://localhost:3000/api/task/{id}
```

Response:
```json
{
  "id": "550e8400-e29b-41d4-a716-446655440000",
  "status": "completed",
  "task": "Create a Python script that prints Hello World",
  "model": "openai/gpt-4.1-mini",
  "iterations": 3,
  "result": "I've created hello.py with a simple Hello World script...",
  "log": [...]
}
```

### Stream Task Progress (SSE)

```bash
curl http://localhost:3000/api/task/{id}/stream
```

Events:
- `log` - Execution log entries (tool calls, results)
- `done` - Task completion with final status

### Health Check

```bash
curl http://localhost:3000/api/health
```

## Available Tools

| Tool | Description |
|------|-------------|
| `read_file` | Read file contents (any path on the machine) with optional line range |
| `write_file` | Write/create files anywhere on the machine |
| `delete_file` | Delete files anywhere on the machine |
| `list_directory` | List directory contents anywhere on the machine |
| `search_files` | Search for files by name pattern (machine-wide; scope with `path`) |
| `run_command` | Execute shell commands (optionally in a specified `cwd`) |
| `grep_search` | Search file contents with regex (machine-wide; scope with `path`) |
| `web_search` | Search the web (DuckDuckGo) |
| `fetch_url` | Fetch URL contents |
| `git_status` | Get git status for any repo path |
| `git_diff` | Show git diff for any repo path |
| `git_commit` | Create git commits for any repo path |
| `git_log` | Show git log for any repo path |

## Configuration

| Variable | Default | Description |
|----------|---------|-------------|
| `OPENROUTER_API_KEY` | (required) | Your OpenRouter API key |
| `DEFAULT_MODEL` | `anthropic/claude-sonnet-4.5` | Default LLM model |
| `WORKING_DIR` | `.` (dev) / `/root` (prod) | Default working directory for relative paths (agent still has full machine access) |
| `HOST` | `127.0.0.1` | Server bind address |
| `PORT` | `3000` | Server port |
| `MAX_ITERATIONS` | `50` | Max agent loop iterations |

## Architecture

```
┌─────────────────┐     ┌─────────────────┐
│   HTTP Client   │────▶│   HTTP API      │
└─────────────────┘     │   (axum)        │
                        └────────┬────────┘
                                 │
                        ┌────────▼────────┐
                        │   Agent Loop    │◀──────┐
                        │                 │       │
                        └────────┬────────┘       │
                                 │                │
                   ┌─────────────┼─────────────┐  │
                   ▼             ▼             ▼  │
            ┌──────────┐  ┌──────────┐  ┌──────────┐
            │   LLM    │  │  Tools   │  │  Tools   │
            │(OpenRouter)│ │(file,git)│ │(term,web)│
            └──────────┘  └──────────┘  └──────────┘
                   │
                   └──────────────────────────────┘
                            (results fed back)
```

## Development

```bash
# Run with debug logging
RUST_LOG=debug cargo run

# Run tests
cargo test

# Format code
cargo fmt

# Check for issues
cargo clippy
```

## Dashboard (Bun)

The dashboard lives in `dashboard/` and uses **Bun** as the package manager.

```bash
cd dashboard
bun install
PORT=3001 bun dev
```

## Calibration (Trial-and-Error Tuning)

Open Agent supports empirical tuning of its **difficulty (complexity)** and **cost** estimation via a calibration harness.

### Run calibrator

```bash
export OPENROUTER_API_KEY="sk-or-v1-..."
cargo run --release --bin calibrate -- --workspace ./.open_agent_calibration --model openai/gpt-4.1-mini --write-tuning
```

This writes a tuning file at `./.open_agent_calibration/.open_agent/tuning.json`. Move/copy it to your real workspace as `./.open_agent/tuning.json` to enable it.

## License

MIT
