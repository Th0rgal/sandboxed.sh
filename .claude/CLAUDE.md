# Open Agent

Minimal autonomous coding agent in Rust with **full machine access** (not sandboxed).

## Quick Reference

| Component | Location | Purpose |
|-----------|----------|---------|
| Backend (Rust) | `src/` | HTTP API + agent system |
| Dashboard (Next.js) | `dashboard/` | Web UI (uses **Bun**, not npm) |
| iOS Dashboard | `ios_dashboard/` | Native iOS app (Swift/SwiftUI) |
| MCP configs | `.open_agent/mcp/config.json` | Model Context Protocol servers |
| Tuning | `.open_agent/tuning.json` | Calibration data |

## Commands

```bash
# Backend
cargo build --release           # Build
cargo run --release             # Run server (port 3000)
RUST_LOG=debug cargo run        # Debug mode
cargo test                      # Run tests
cargo fmt                       # Format code
cargo clippy                    # Lint

# Dashboard (uses Bun, NOT npm)
cd dashboard
bun install                     # Install deps
bun dev                         # Dev server (port 3001)
bun run build                   # Production build

# Deployment
ssh root@95.216.112.253 'cd /root/open_agent && git pull && cargo build --release && cp target/release/open_agent /usr/local/bin/ && systemctl restart open_agent'
```

## Architecture

```
SimpleAgent
    └── TaskExecutor → runs tools in a loop with auto-upgrade
```

### Module Map

```
src/
├── agents/           # Agent system
│   ├── simple.rs     # SimpleAgent (main entry point)
│   └── leaf/         # TaskExecutor
├── budget/           # Cost tracking, pricing, smart retry
│   ├── benchmarks.rs # Model capability scores
│   ├── pricing.rs    # OpenRouter pricing + model allowlist
│   └── resolver.rs   # Model family auto-upgrade system
├── memory/           # Supabase + pgvector persistence
│   ├── supabase.rs   # Database client
│   ├── context.rs    # ContextBuilder, SessionContext
│   ├── retriever.rs  # Semantic search
│   └── writer.rs     # Event recording
├── mcp/              # MCP server registry + config
├── llm/              # OpenRouter client
├── tools/            # File ops, terminal, git, web, search, desktop
├── task/             # Task types + verification
├── config.rs         # Config + env vars
└── api/              # HTTP routes (axum)
```

## Execution Backends

Open Agent supports two execution backends:

### Local Backend (default without OpenCode)
Uses the built-in agent loop with OpenRouter for LLM access.

### OpenCode Backend (recommended for Claude Max)
Delegates task execution to an external OpenCode server, enabling use of Claude Max subscription.

```bash
# Enable OpenCode backend
AGENT_BACKEND=opencode
OPENCODE_BASE_URL=http://127.0.0.1:4096
OPENCODE_AGENT=build
OPENCODE_PERMISSIVE=true
```

**Architecture with OpenCode:**
```
Dashboard → Open Agent API → OpenCode Server → Anthropic API (Claude Max)
```

## Model Preferences

**With OpenCode backend:** Use Claude models via your Claude Max subscription.
- `anthropic/claude-opus-4-5-20251101` - Most capable, recommended
- `anthropic/claude-sonnet-4-20250514` - Good balance of speed/capability

**With Local backend (OpenRouter):**
1. `google/gemini-3-flash-preview` - Fast, cheap, excellent tool use
2. `qwen/qwen3-235b-a22b-instruct` - Strong reasoning, affordable
3. `deepseek/deepseek-v3.2` - Good value, capable

## API Endpoints

| Method | Path | Purpose |
|--------|------|---------|
| `POST` | `/api/task` | Submit task |
| `GET` | `/api/task/{id}` | Get status |
| `GET` | `/api/task/{id}/stream` | SSE progress |
| `GET` | `/api/health` | Health check |
| `POST` | `/api/control/message` | Send message to agent |
| `GET` | `/api/control/stream` | SSE event stream |
| `GET` | `/api/models` | List available models |

## Environment Variables

### Required
| Variable | Description |
|----------|-------------|
| `OPENROUTER_API_KEY` | OpenRouter API key (sk-or-v1-...) |

### Production Auth
| Variable | Description |
|----------|-------------|
| `DEV_MODE` | `true` bypasses auth |
| `DASHBOARD_PASSWORD` | Password for dashboard login |
| `JWT_SECRET` | HMAC secret for JWT signing |

### Optional
| Variable | Default | Description |
|----------|---------|-------------|
| `DEFAULT_MODEL` | `google/gemini-3-flash-preview` | Default LLM |
| `WORKING_DIR` | `/root` (prod), `.` (dev) | Working directory |
| `HOST` | `127.0.0.1` | Bind address |
| `PORT` | `3000` | Server port |
| `MAX_ITERATIONS` | `50` | Max agent loop iterations |
| `SUPABASE_URL` | - | Supabase project URL |
| `SUPABASE_SERVICE_ROLE_KEY` | - | Service role key |
| `TAVILY_API_KEY` | - | Web search API key |

## Secrets

Use `secrets.json` (gitignored) for local development. Template: `secrets.json.example`

```bash
# Read secrets
jq -r '.openrouter.api_key' secrets.json
```

**Rules:**
- Never paste secret values into code, comments, or docs
- Read secrets from environment variables at runtime

## Code Conventions

### Rust - Provability-First Design

Code should be written as if we want to **formally prove it correct later**. This means:

1. **Never panic** - always return `Result<T, E>`
2. **Exhaustive matches** - no `_` catch-all patterns in enums (forces handling new variants)
3. **Document invariants** as `/// Precondition:` and `/// Postcondition:` comments
4. **Pure functions** - separate pure logic from IO where possible
5. **Algebraic types** - prefer enums with exhaustive matching over stringly-typed data
6. Costs are in **cents (u64)** - never use floats for money

```rust
// Use thiserror for error types
#[derive(Debug, Error)]
pub enum MyError {
    #[error("description: {0}")]
    Variant(String),
}

// Propagate with ?
pub fn do_thing() -> Result<T, MyError> {
    let x = fallible_op()?;
    Ok(x)
}
```

### Adding a New Tool

1. Add to `src/tools/` (new file or extend existing)
2. Implement `Tool` trait: `name()`, `description()`, `parameters()`, `call()`
3. Register in `src/tools/mod.rs` → `create_tools()`
4. Tool parameters use serde_json schema format
5. Document pre/postconditions for provability

### Dashboard (Next.js + Bun)
- Package manager: **Bun** (not npm/yarn/pnpm)
- Icons: **Lucide React** (`lucide-react`)
- API base: `process.env.NEXT_PUBLIC_API_URL ?? 'http://127.0.0.1:3000'`
- Auth: JWT stored in `sessionStorage`

### Design System - "Quiet Luxury + Liquid Glass"
- **Dark-first** aesthetic (dark mode is default)
- No pure black - use deep charcoal (#121214)
- Elevation via color, not shadows
- Use `white/[opacity]` for text (e.g., `text-white/80`)
- Accent color: indigo-500 (#6366F1)
- Borders: very subtle (0.06-0.08 opacity)
- No bounce animations, use `ease-out`

## Production

| Property | Value |
|----------|-------|
| Host | `95.216.112.253` |
| SSH | `ssh root@95.216.112.253` |
| Backend URL | `https://agent-backend.thomas.md` |
| Dashboard URL | `https://agent.thomas.md` |
| Binary | `/usr/local/bin/open_agent` |
| Env file | `/etc/open_agent/open_agent.env` |
| Service | `systemctl status open_agent` |

## Adding New Components

### New API Endpoint
1. Add handler in `src/api/`
2. Register route in `src/api/mod.rs`
3. Update this doc

### After Significant Changes
- Update `.cursor/rules/` if architecture changes
- Update `CLAUDE.md` for new env vars or commands
