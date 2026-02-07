# Debugging Sandboxed.sh

## Remote Servers

| Server     | SSH                                        | Domain                          |
| ---------- | ------------------------------------------ | ------------------------------- |
| **Thomas** | `ssh -i ~/.ssh/cursor root@95.216.112.253` | https://agent-backend.thomas.md |
| **Ben**    | `ssh -i ~/.ssh/cursor root@88.99.4.254`    | https://fricobackend.relens.ai  |

## Backend Services (Thomas)

Thomas's server runs two instances:

| Service | Port | Domain | Binary |
|---------|------|--------|--------|
| `sandboxed-sh-prod` | 3000 | agent-backend.thomas.md | `/usr/local/bin/sandboxed-sh-prod` |
| `sandboxed-sh-dev` | 3002 | agent-backend-dev.thomas.md | `/usr/local/bin/sandboxed-sh-dev` |

```bash
# Production
systemctl status sandboxed-sh-prod
journalctl -u sandboxed-sh-prod -f
systemctl restart sandboxed-sh-prod

# Development
systemctl status sandboxed-sh-dev
journalctl -u sandboxed-sh-dev -f
systemctl restart sandboxed-sh-dev
```

**Paths (Production):**

- Binary: `/usr/local/bin/sandboxed-sh-prod`
- Config: `/etc/sandboxed_sh/sandboxed_sh.env`
- Service: `/etc/systemd/system/sandboxed-sh-prod.service`
- Data: `/root/.sandboxed-sh/`

**Paths (Development):**

- Binary: `/usr/local/bin/sandboxed-sh-dev`
- Config: `/etc/sandboxed_sh/sandboxed_sh_dev.env`
- Service: `/etc/systemd/system/sandboxed-sh-dev.service`
- Data: `/root/.sandboxed-sh-dev/`

## Backend Service (Ben)

Ben's server runs a single instance:

```bash
systemctl status sandboxed-sh
journalctl -u sandboxed-sh -f
systemctl restart sandboxed-sh
```

## Dashboard

Run the dashboard locally and point it to any backend:

```bash
cd dashboard
bun install
bun run dev   # Runs on http://localhost:3001
```

Configure the backend URL in the dashboard settings or environment to connect to
Thomas's or Ben's server.

## Deploying Updates

**Always use debug builds** (`cargo build`) instead of release (`cargo build --release`).
Debug builds compile in ~1 minute vs ~5+ minutes for release. The performance
difference is negligible for this I/O-bound service.

### ⚠️ Cross-Platform Warning

**Never copy a locally-built binary to the server if you're on macOS.** The binary
format differs between platforms:
- macOS produces Mach-O executables (won't run on Linux)
- Linux requires ELF executables

If you copy a macOS binary, the service will fail with `exit code 203/EXEC`.

### Recommended: Build on Server

Sync source code and build directly on the server:

```bash
# Sync source (backend-only; avoids copying dashboard/ which deploys via Vercel)
rsync -avz --exclude 'target' --exclude '.git' --exclude 'dashboard' \
  -e "ssh -i ~/.ssh/cursor" \
  /Users/thomas/work/open_agent/ root@95.216.112.253:/opt/sandboxed-sh-dev/

# If you need the dashboard on the server for debugging, remove the dashboard exclude:
# rsync -avz --exclude 'target' --exclude '.git' --exclude 'dashboard/node_modules' --exclude 'dashboard/.next' \
#   -e "ssh -i ~/.ssh/cursor" \
#   /Users/thomas/work/open_agent/ root@95.216.112.253:/opt/sandboxed-sh-dev/

# Build on server (debug mode)
ssh -i ~/.ssh/cursor root@95.216.112.253 "cd /opt/sandboxed-sh-dev && source ~/.cargo/env && cargo build"

# Copy binaries (main + MCP tools) and restart
# Note: stop service first to avoid "Text file busy" when replacing MCP binaries.
ssh -i ~/.ssh/cursor root@95.216.112.253 "systemctl stop sandboxed-sh-dev && \
  cp /opt/sandboxed-sh-dev/target/debug/sandboxed-sh /usr/local/bin/sandboxed-sh-dev && \
  cp /opt/sandboxed-sh-dev/target/debug/workspace-mcp /usr/local/bin/ && \
  cp /opt/sandboxed-sh-dev/target/debug/desktop-mcp /usr/local/bin/ && \
  systemctl start sandboxed-sh-dev"

# Verify health
curl https://agent-backend-dev.thomas.md/api/health
```

### Alternative: Cross-compile (if you have cross set up)

```bash
# Build (debug mode - always use this)
cargo build

# Deploy to Thomas (dev first, then promote to prod)
scp -i ~/.ssh/cursor target/debug/sandboxed_sh root@95.216.112.253:/usr/local/bin/sandboxed-sh-dev
ssh -i ~/.ssh/cursor root@95.216.112.253 "systemctl restart sandboxed-sh-dev"

# After testing, promote to production
ssh -i ~/.ssh/cursor root@95.216.112.253 "cp /usr/local/bin/sandboxed-sh-dev /usr/local/bin/sandboxed-sh-prod && systemctl restart sandboxed-sh-prod"

# Deploy to Ben
scp -i ~/.ssh/cursor target/debug/sandboxed_sh root@88.99.4.254:/usr/local/bin/sandboxed-sh
ssh -i ~/.ssh/cursor root@88.99.4.254 "systemctl restart sandboxed-sh"
```

**Faster compilation tips:**
- Use `cargo check` for syntax/type validation without producing a binary
- Build on the server (has 8 cores) rather than transferring the binary
- Install `sccache` for caching compiled dependencies across builds

## Mission Database

Located at `~/.sandboxed-sh/missions/missions.db` on the server.

```bash
# Query via API
curl "https://agent-backend.thomas.md/api/control/missions/<id>/events" \
  -H "Authorization: Bearer <token>"

# Direct access
sqlite3 ~/.sandboxed-sh/missions/missions.db "SELECT id, status, created_at FROM missions ORDER BY created_at DESC LIMIT 10;"
```

## Troubleshooting

**Service won't start (exit code 203/EXEC):** Usually means wrong binary architecture (e.g., macOS ARM binary on Linux). Build on server instead.

**MCPs show "Failed to spawn process" error:** The MCP binaries (`workspace-mcp`, `desktop-mcp`) need to be installed to `/usr/local/bin/`. After building, copy them:
```bash
ssh -i ~/.ssh/cursor root@95.216.112.253 "cp /opt/sandboxed-sh-dev/target/debug/workspace-mcp /usr/local/bin/ && \
  cp /opt/sandboxed-sh-dev/target/debug/desktop-mcp /usr/local/bin/"
```
Then restart the backend. Workspace-scoped MCPs must be in PATH for both host workspace missions and the Extensions page to work.

**Config profile not being applied:** Check that:
1. The configs exist in the **correct library path** (`.sandboxed-sh/library/` for production, not `.openagent/library/` which was the old path)
2. The library has the `configs/<profile>/.opencode/oh-my-opencode.json` file
3. Pull latest library: `cd ~/.sandboxed-sh/library && git pull`

**Missions not using correct settings:**

**Missions stuck:** Look for running CLI processes:

```bash
ps aux | grep -E "claude|oh-my-opencode"
machinectl list   # For container workspaces
```

**Proxy issues:**

- Thomas uses nginx: `/etc/nginx/sites-available/`
- Ben uses Caddy: `/etc/caddy/Caddyfile`
