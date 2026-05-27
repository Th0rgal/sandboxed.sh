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

### Investigating Unexpected Service Stops

When a mission reports `SIGTERM` or `SIGKILL`, first determine whether the
backend was deliberately stopped by systemd:

```bash
journalctl -u sandboxed-sh-prod \
  --since "2026-04-18 06:00:00 UTC" \
  --until "2026-04-18 06:10:00 UTC" \
  --no-pager | grep -Ei "Shutdown signal|Stopping|Stopped|Started|SIGTERM|SIGKILL|client request"
```

If systemd says `on client request`, correlate that timestamp with SSH sessions:

```bash
journalctl \
  --since "2026-04-18 06:00:00 UTC" \
  --until "2026-04-18 06:10:00 UTC" \
  --no-pager | grep -Ei "Accepted publickey|session-|sshd|sandboxed-sh-prod"
```

The app can log that it received `SIGTERM`/`SIGINT`, process metadata, and
interrupted mission IDs, but Linux does not pass the exact `systemctl` caller to
the process. For command-level attribution, enable host auditing for `systemctl`
before reproducing:

```bash
apt-get install -y auditd
auditctl -a always,exit -F arch=b64 -S execve -F path=/usr/bin/systemctl -k systemctl_exec
auditctl -a always,exit -F arch=b64 -S execve -F path=/bin/systemctl -k systemctl_exec
ausearch -k systemctl_exec --start "2026-04-18 06:00:00" --end "2026-04-18 06:10:00"
```

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
  cp /opt/sandboxed-sh-dev/target/debug/orchestrator-mcp /usr/local/bin/ && \
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

## Hot-Swapping via the MCP (preferred over raw systemctl)

Missions should never call `systemctl restart sandboxed-sh-prod` directly.
Each restart SIGTERMs every codex/claude session under the service, which
auto-resumes from the backend but loses the in-flight turn's progress.
Worse, an agent in a retry loop can chainsaw the host (the original
incident: 5 deploys + 2 rollbacks in 90 minutes).

The orchestrator MCP exposes a `deploy_sandboxed_sh` tool that hits the
internal `/api/system/deploy` endpoint with the safety rails:

- **Self-protection** — refuses by default if the calling mission lives on
  the service being restarted. Override with `force=true` only if you
  accept that your own turn will be SIGTERM'd.
- **Debounce** — refuses if another deploy fired in the last 5 minutes.
  Override with `force=true`.
- **Atomic install** via versioned symlink swap + a detached
  `setsid`/`nohup` restart so the response flushes before the service
  dies.

From inside an agent that has the orchestrator MCP, the call shape is:

```jsonc
{
  "tool": "deploy_sandboxed_sh",
  "arguments": {
    "git_ref": "origin/master",   // optional; omit to use current checkout
    "skip_build": false,          // optional; true if you built elsewhere
    "force": false                // override safety rails
  }
}
```

Successful response:

```jsonc
{
  "deployed": true,
  "summary": "Deployed commit abc123def456; service sandboxed-sh-prod will restart in ~2s",
  "logs": ["Building sandboxed-sh-prod ...", "Build complete", ...]
}
```

A refusal (HTTP 409 self-target or 429 debounce) surfaces a message that
explains the override knob, so an agent can decide whether to retry with
`force=true` or back off.

## Locking down the SSH backdoor

Mission containers ship with an SSH private key (`SSH_PRIVATE_KEY_B64` in
their env) whose public counterpart is in `/root/.ssh/authorized_keys` on
the host. Agents have been using this to `ssh root@<host> "systemctl
restart ..."`, which bypasses the MCP's safety rails. Restrict the key to
the one command we actually want it to run.

### Step 1 — identify the agent key

```bash
ssh -i ~/.ssh/cursor root@95.216.112.253 \
  'cat /root/.ssh/authorized_keys'
```

The agent key is the one whose fingerprint matches the `SSH_PRIVATE_KEY_B64`
baked into mission containers (it's a fixed key, not per-mission). Look
for the entry that has *no* `command="..."` restriction and isn't your own
`cursor` admin key.

### Step 2 — restrict it

Edit `/root/.ssh/authorized_keys` and prepend the agent key entry with the
following options (one line, no leading whitespace):

```
command="/usr/local/bin/sandboxed-agent-ssh",no-agent-forwarding,no-port-forwarding,no-X11-forwarding,no-pty <ssh-ed25519 ...>
```

The `command=` clause makes SSH **only** run that program, regardless of
what the client requested. The remaining flags drop attack surface.

### Step 3 — create the dispatcher

`/usr/local/bin/sandboxed-agent-ssh` should be a small shell script that
accepts a tightly-scoped set of "subcommands" (e.g. log tailing, mission
status queries) and rejects everything else. **Deploy is intentionally
NOT in this list** — agents must go through the MCP for that.

A minimal version:

```bash
#!/usr/bin/env bash
set -euo pipefail
case "${SSH_ORIGINAL_COMMAND:-}" in
  "status:prod")
    systemctl is-active sandboxed-sh-prod
    ;;
  "status:dev")
    systemctl is-active sandboxed-sh-dev
    ;;
  "logs:prod:"*)
    n="${SSH_ORIGINAL_COMMAND#logs:prod:}"
    journalctl -u sandboxed-sh-prod --no-pager -n "${n:-100}"
    ;;
  *)
    echo "agent-ssh: command refused: ${SSH_ORIGINAL_COMMAND:-<none>}" >&2
    exit 126
    ;;
esac
```

Make it executable: `chmod +x /usr/local/bin/sandboxed-agent-ssh`.

### Step 4 — verify

From a mission container shell:

```bash
ssh -o StrictHostKeyChecking=no root@<host> "systemctl restart sandboxed-sh-prod"
# expect: "agent-ssh: command refused: systemctl restart sandboxed-sh-prod"
# expect: exit 126

ssh -o StrictHostKeyChecking=no root@<host> "status:prod"
# expect: "active"
```

After this lands, all deploys go through the MCP and the chainsaw incident
can't recur.

## Mission Database

Located at `~/.sandboxed-sh/missions/missions.db` on the server.

```bash
# Query via API
curl "https://agent-backend.thomas.md/api/control/missions/<id>/events" \
  -H "Authorization: Bearer <token>"

# Direct access
sqlite3 ~/.sandboxed-sh/missions/missions.db "SELECT id, status, created_at FROM missions ORDER BY created_at DESC LIMIT 10;"
```

## Mission Streaming Smoke Test

Use the manual smoke test script to validate streaming behavior for Claude Code,
OpenCode, and Codex against the **development** backend. This is intentionally
not part of CI.

1. Copy the example env file and fill in values:
```
cp scripts/mission_stream_smoke.env.example .env.local
```

2. Export the variables (or source `.env.local`):
```
set -a
source .env.local
set +a
```

3. Run the smoke test (all backends):
```
python3 scripts/mission_stream_smoke.py
```

Optional flags:
- `--backend claudecode` (repeatable; limit to one backend)
- `--timeout 180` (seconds per backend)
- `--allow-no-thinking` (skip the thinking requirement)
- `--verbose` (print streamed events)

Expected behavior per backend:
- First message triggers tool calls + results
- Second message is queued (`queued: true`)

## Codex Model Effort Check

To verify Codex `model_effort` end-to-end on dev:

```bash
# Create mission with Codex effort override
curl -sS -X POST "https://agent-backend-dev.thomas.md/api/control/missions" \
  -H "Content-Type: application/json" \
  -d '{
    "title": "Codex effort smoke",
    "backend": "codex",
    "model_override": "gpt-5-codex",
    "model_effort": "high"
  }'
```

Then load the mission in the dashboard and send a message. The mission metadata
should show `model_effort: high`.
- Stream includes `thinking`, `text_delta`, `tool_call`, `tool_result`, and `assistant_message`

## Troubleshooting

**Service won't start (exit code 203/EXEC):** Usually means wrong binary architecture (e.g., macOS ARM binary on Linux). Build on server instead.

**MCPs show "Failed to spawn process" error:** The MCP binaries (`workspace-mcp`, `desktop-mcp`, `orchestrator-mcp`) need to be installed to `/usr/local/bin/`. After building, copy them:
```bash
ssh -i ~/.ssh/cursor root@95.216.112.253 "cp /opt/sandboxed-sh-dev/target/debug/workspace-mcp /usr/local/bin/ && \
  cp /opt/sandboxed-sh-dev/target/debug/desktop-mcp /usr/local/bin/ && \
  cp /opt/sandboxed-sh-dev/target/debug/orchestrator-mcp /usr/local/bin/"
```
Then restart the backend. Workspace-scoped MCPs must be in PATH for both host workspace missions and the Extensions page to work.

**Config profile not being applied:** Check that:
1. The configs exist in the **correct library path** (`.sandboxed-sh/library/` for production, not `.openagent/library/` which was the old path)
2. The library has the expected `configs/<profile>/.opencode/` files
3. Pull latest library: `cd ~/.sandboxed-sh/library && git pull`

**Missions not using correct settings:**

**Missions stuck:** Look for running CLI processes:

```bash
ps aux | grep -E "claude|opencode"
machinectl list   # For container workspaces
```

**Proxy issues:**

- Thomas uses nginx: `/etc/nginx/sites-available/`
- Ben uses Caddy: `/etc/caddy/Caddyfile`

## Mission Debug Runbook

When a mission fails or behaves unexpectedly, export a reproducible debug bundle first.

### 1) Export a mission debug bundle

```bash
scripts/mission_debug_bundle.sh \
  --base-url "https://agent-backend-dev.thomas.md" \
  --token "<control-api-token>" \
  --mission-id "<mission-uuid>"
```

Optional tuning:
- `--max-events 5000` for longer missions
- `--page-size 500` for fewer API round-trips
- `--out-dir /tmp/mission-bundles` to write elsewhere

Output archive:
- `output/debug-bundles/mission-debug-<mission-id>-<timestamp>.tar.gz`

### 2) Inspect key files in the bundle

- `bundle_meta.json`: confirms export coverage and request failures.
- `mission_summary.json`: mission status, backend, and `terminal_reason`.
- `events_summary.json`: event type counts, first/last timestamps, error excerpts.
- `raw/mission.json`: full mission object including history metadata.
- `raw/progress.json`: latest runtime progress snapshot.
- `raw/opencode_diagnostics.json`: OpenCode runtime diagnostic mode/status.
- `raw/events/page-*.json`: paginated event history used for replay/triage.

### 3) Triage metrics checklist

Validate these metrics first before deeper log spelunking:

1. `terminal_reason` (`mission_summary.json`)
2. Event distribution (`events_summary.json.by_type`)
3. Last protocol activity (`events_summary.json.last_timestamp`)
4. Presence of `error` events (`events_summary.json.terminal_error_events`)
5. Active run state (`raw/progress.json`)

### 4) Quick API commands (without bundle)

```bash
# Mission snapshot
curl -sS "https://agent-backend-dev.thomas.md/api/control/missions/<mission-id>" \
  -H "Authorization: Bearer <token>" | jq

# Recent mission events
curl -sS "https://agent-backend-dev.thomas.md/api/control/missions/<mission-id>/events?limit=100&offset=0" \
  -H "Authorization: Bearer <token>" | jq

# Runtime progress + diagnostics
curl -sS "https://agent-backend-dev.thomas.md/api/control/progress" \
  -H "Authorization: Bearer <token>" | jq
curl -sS "https://agent-backend-dev.thomas.md/api/control/diagnostics/opencode" \
  -H "Authorization: Bearer <token>" | jq
```
