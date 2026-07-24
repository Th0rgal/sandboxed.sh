# Debugging Sandboxed.sh

## Remote Servers

| Server | SSH | Domain |
| --- | --- | --- |
| **Thomas / agent-core** | `ssh -i ~/.ssh/paloma root@65.109.98.246` | https://agent-backend.thomas.md |
| **Thomas / old-agent compute** | `ssh -i ~/.ssh/paloma root@95.216.112.253` | Sandboxed node only |
| **Ben** | `ssh -i ~/.ssh/paloma root@88.99.4.254` | https://fricobackend.relens.ai |

## Backend Services (Thomas / agent-core)

`agent-core` (`65.109.98.246`, Tailscale `100.107.113.19`) is the only Thomas
control plane. It runs the two sandboxed.sh instances plus the Hermes assistant
stack. Hermes's server inventory calls it **`agent-core`**; the historical
`sepolia.rpc.starknet.id` name is only a compatibility DNS alias.

`old-agent` (`95.216.112.253`) is intentionally still powered on, but only as a
leaf compute host. Its Sandboxed node ID is **`old-agent`** and exposes port
3088 only to `65.109.98.246`. Its former sandboxed.sh, Hermes, fleet-daemon, and
nginx control-plane services remain stopped and disabled. Do not use it as a
scheduler, relay, or rollback control plane.

Network routing is direct:

- `agent-core` reaches DGX Spark over Tailscale at `100.77.4.93:3088`. There is
  no production SSH tunnel through `old-agent`.
- Ashur, Babylon, Nippur, and `old-agent` expose their Sandboxed node API only
  to the public address `65.109.98.246`.
- The Paloma SSH key on `agent-core` is authorized for operational reads on
  Ashur, Babylon, Nippur, DGX, and `old-agent`; `disk-sentinel` exercises these
  direct paths.
- DNS already points the production endpoints at `agent-core`. The current
  Cloudflare automation token still returns `Invalid API Token`; rotate it
  before the next DNS mutation. Do not reintroduce an `old-agent` relay as a
  workaround.

### Disk sentinel

The production cron runs:

```text
/srv/sandboxed-storage/staging/agent-core/hermes/.hermes/scripts/disk-sentinel.sh
```

Keep its mirrored operator copy in
`/srv/sandboxed-storage/staging/agent-core/hermes/scripts/` identical. It checks
`agent-core` locally, then Ashur, Babylon, Nippur, DGX Spark, and `old-agent`
over direct SSH. It must not contain the retired CI runner or route DGX through
`old-agent`.

Remote SSH is deliberately non-interactive and fail-closed:

- Ashur, Babylon, Nippur, and `old-agent` use
  `/var/lib/hermes-assistant/.ssh/paloma`.
- DGX uses `/root/.ssh/agent-core-dgx-tunnel` directly; despite the historical
  filename, this is now a direct Tailscale SSH identity.
- Every host key must already be pinned in `/root/.ssh/known_hosts`; do not
  solve a sentinel incident with `StrictHostKeyChecking=no`.

When several hosts suddenly report `DISK UNKNOWN`, first execute the script
manually on `agent-core`. Check the explicit identity path and pinned host key
for each failing entry before diagnosing a simultaneous fleet outage:

```bash
ssh -i ~/.ssh/paloma root@65.109.98.246 \
  '/srv/sandboxed-storage/staging/agent-core/hermes/.hermes/scripts/disk-sentinel.sh'
```

No output means every monitored filesystem is reachable and below its alert
threshold. The current cron job ID is `6bfc537e6dc9`; a manual cron execution
must also finish successfully before closing the incident.

DGX is the one sentinel target that depends on the control plane's Tailscale
service. If only `dgx-spark` is unknown, verify the route before changing SSH
keys or adding a relay:

```bash
systemctl is-enabled tailscaled
systemctl is-active tailscaled
tailscale ping -c 2 100.77.4.93
```

`tailscaled` must be both enabled and active on `agent-core`. Recover the
expected persisted service with `systemctl enable --now tailscaled`, then
require all three checks to pass: Tailscale ping, strict-host-key SSH to DGX,
and a silent `disk-sentinel.sh` run. Never restore the retired `old-agent`
tunnel to mask a stopped local Tailscale service.

| Service | Port | Domain | Binary |
|---------|------|--------|--------|
| `sandboxed-sh-prod` | 3000 | agent-backend.thomas.md | `/usr/local/bin/sandboxed-sh-prod` |
| `sandboxed-sh-dev` | 3002 | agent-backend-dev.thomas.md | `/usr/local/bin/sandboxed-sh-dev` |
| `hermes-assistant` | 8642 (loopback) | agent-backend.thomas.md `/hermes-remote` | `/usr/local/bin/hermes` (gateway) |
| `hermes-dashboard` | 9130 (loopback) | agent-backend.thomas.md `/hermes-desktop` | `/usr/local/bin/hermes` (dashboard) |

**Hermes endpoints** (nginx config in
`/etc/nginx/sites-enabled/agent-backend.thomas.md` — NOT tracked in git):

- `/hermes-remote` → the gateway's OpenAI-compatible API server (loopback 8642),
  re-exposed by the sandboxed.sh backend itself (`hermes_remote_proxy` in
  `src/api/system.rs`). `/v1/*` + SSE only, no WebSocket. Used for *split mode*
  (a local Hermes sets `GATEWAY_PROXY_URL`/`GATEWAY_PROXY_KEY` and delegates
  agent work). Key = `API_SERVER_KEY` in `/etc/sandboxed-sh/hermes-assistant.env`,
  readable via `GET /api/system/hermes-assistant/remote`.
- `/hermes-desktop` → the full Hermes dashboard backend (loopback 9130) for the
  desktop app's **native Remote-gateway mode** (`/api/status` + `wss /api/ws`).
  Unit: `/etc/systemd/system/hermes-dashboard.service` (`hermes dashboard
  --no-open --host 127.0.0.1 --port 9130 --skip-build`, same `HERMES_HOME`
  `/var/lib/hermes-assistant` as the gateway so sessions/memory are shared,
  auth via `HERMES_DASHBOARD_SESSION_TOKEN` = same key, production web dist at
  `/var/lib/hermes-assistant/web_dist`). The nginx location pins
  `Host 127.0.0.1:9130` (host-header check) and must NOT add `X-Forwarded-For`
  (the WS gate requires a loopback peer).

Hermes gotchas:

- After a prod deploy, `systemctl restart hermes-assistant` so the gateway
  respawns the freshly installed `/usr/local/bin/assistant-mcp`.
- Companion binaries are environment-scoped: production owns the historical
  unsuffixed paths (`assistant-mcp`, `orchestrator-mcp`, `palomactl`), while
  `sandboxed-sh-dev` installs `*-dev`. Never copy a dev MCP over an unsuffixed
  production path; doing so changes Hermes production tooling without a prod
  deploy.
- When testing the WS handshake with curl, force `--http1.1` — ALPN negotiates
  h2, which has no `Upgrade` header, and the resulting 401 is a red herring.
- When rotating the `hsk_` key, update `API_SERVER_KEY`,
  `HERMES_DASHBOARD_SESSION_TOKEN`, and the desktop's `connection.json` together.

```bash
# Production inspection. Deploy/restart through the guarded endpoint below.
systemctl status sandboxed-sh-prod
journalctl -u sandboxed-sh-prod -f

# Development
systemctl status sandboxed-sh-dev
journalctl -u sandboxed-sh-dev -f
systemctl restart sandboxed-sh-dev
```

**Paths (Production):**

- Binary: `/usr/local/bin/sandboxed-sh-prod`
- Config: `/etc/open_agent/open_agent.env`
- Service: `/etc/systemd/system/sandboxed-sh-prod.service`
- Data root: `/srv/sandboxed-storage/staging/agent-core`

**Paths (Development):**

- Binary: `/usr/local/bin/sandboxed-sh-dev`
- Config: `/etc/open_agent/open_agent_dev.env`
- Service: `/etc/systemd/system/sandboxed-sh-dev.service`
- Data root: inspect `WORKING_DIR`/storage settings in the dev environment file;
  never infer it from the production path.

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
  -e "ssh -i ~/.ssh/paloma" \
  /Users/thomas/work/open_agent/ root@65.109.98.246:/opt/sandboxed-sh-dev/

# If you need the dashboard on the server for debugging, remove the dashboard exclude:
# rsync -avz --exclude 'target' --exclude '.git' --exclude 'dashboard/node_modules' --exclude 'dashboard/.next' \
#   -e "ssh -i ~/.ssh/paloma" \
#   /Users/thomas/work/open_agent/ root@65.109.98.246:/opt/sandboxed-sh-dev/

# Build on server (debug mode)
ssh -i ~/.ssh/paloma root@65.109.98.246 "cd /opt/sandboxed-sh-dev && source ~/.cargo/env && cargo build"

# Copy binaries (main + MCP tools) and restart
# Note: stop service first to avoid "Text file busy" when replacing MCP binaries.
ssh -i ~/.ssh/paloma root@65.109.98.246 "systemctl stop sandboxed-sh-dev && \
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

# Deploy to Thomas / agent-core (dev first, then promote to prod)
scp -i ~/.ssh/paloma target/debug/sandboxed_sh root@65.109.98.246:/usr/local/bin/sandboxed-sh-dev
ssh -i ~/.ssh/paloma root@65.109.98.246 "systemctl restart sandboxed-sh-dev"

# After testing, deploy production through POST /api/system/deploy with
# target_environment=prod and expected_service=sandboxed-sh-prod.service.
# Never copy over the live production binary or run an unguarded raw restart.

# Deploy to Ben
scp -i ~/.ssh/paloma target/debug/sandboxed_sh root@88.99.4.254:/usr/local/bin/sandboxed-sh
ssh -i ~/.ssh/paloma root@88.99.4.254 "systemctl restart sandboxed-sh"
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

The orchestrator MCP exposes a `deploy_sandboxed_sh` tool that hits an
internal `/api/system/deploy` endpoint with safety rails.

**Targeting rule:** `deploy_sandboxed_sh` deploys the backend API it is sent
to. If `target_environment` is omitted, the tool uses the API that launched the
mission. A prod mission therefore deploys prod by default; a dev mission deploys
dev by default. To deploy a different Sandboxed.sh environment, set
`target_environment` explicitly:

| Target | Tool argument | API URL | Service restarted |
|--------|---------------|---------|-------------------|
| Current mission API | omit `target_environment` | mission `API_URL` | current API's service |
| Production | `"prod"` | `http://127.0.0.1:3000` | `sandboxed-sh-prod.service` |
| Development | `"dev"` | `http://127.0.0.1:3002` | `sandboxed-sh-dev.service` |

The backend also receives an `expected_service` guard from the MCP and refuses
with HTTP 409 if the request reaches the wrong service. This prevents a prod
mission that intends to test dev from accidentally deploying prod.

Safety rails:

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
    "target_environment": "dev",  // optional; "dev" or "prod"; omit for current API
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

A refusal surfaces a message that explains the next step:

- HTTP 409 target mismatch: the request reached the wrong service/API URL.
- HTTP 409 self-target: pass `force=true` only if killing the caller is acceptable.
- HTTP 429 debounce: wait or pass `force=true` for an urgent deploy.

For a dev-only restart without rebuilding, use the explicit service command:

```bash
ssh -i ~/.ssh/paloma root@65.109.98.246 "systemctl restart sandboxed-sh-dev"
```

Do not call `deploy_sandboxed_sh` from a prod mission unless you intend to
deploy a Sandboxed.sh backend. For dev testing from prod, prefer
`target_environment: "dev"` so the request goes to the dev API and carries the
`sandboxed-sh-dev.service` guard.

## Locking down the SSH backdoor

Mission containers ship with an SSH private key (`SSH_PRIVATE_KEY_B64` in
their env) whose public counterpart is in `/root/.ssh/authorized_keys` on
the host. Agents have been using this to `ssh root@<host> "systemctl
restart ..."`, which bypasses the MCP's safety rails. Restrict the key to
the one command we actually want it to run.

### Step 1 — identify the agent key

```bash
ssh -i ~/.ssh/paloma root@65.109.98.246 \
  'cat /root/.ssh/authorized_keys'
```

The agent key is the one whose fingerprint matches the `SSH_PRIVATE_KEY_B64`
baked into mission containers (it's a fixed key, not per-mission). Look
for the entry that has *no* `command="..."` restriction and isn't your own
`paloma` admin key.

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
ssh -i ~/.ssh/paloma root@65.109.98.246 "cp /opt/sandboxed-sh-dev/target/debug/workspace-mcp /usr/local/bin/ && \
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

- Thomas / agent-core uses nginx: `/etc/nginx/sites-available/`
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

## Resource Isolation & Host Overload (cgroups: memory + CPU)

Symptom: the whole server feels overloaded, dashboards lag, and harnesses can
barely stream — usually while a mission runs a heavy fan-out (Lean/proof or
contract builds). Diagnose **what kind** of pressure it is before touching
anything:

```bash
uptime; nproc                       # load average vs core count
free -h                             # is memory/swap the problem?
cat /proc/pressure/cpu              # `some avg10` high + load > nproc => CPU-bound
cat /proc/pressure/memory /proc/pressure/io
ps -eo pid,pcpu,ni,comm --sort=-pcpu | head -20
```

If it's **CPU**, confirm the cgroup tiering is actually in effect:

```bash
# All three should NOT be equal — missions.slice must be < system.slice.
for s in missions.slice system.slice user.slice; do
  echo -n "$s cpu.weight="; cat /sys/fs/cgroup/$s/cpu.weight 2>/dev/null
done
systemctl show sandboxed-sh-prod -p CPUWeight -p Slice   # API tier weight
systemctl show missions.slice -p CPUWeight -p CPUQuotaPerSecUSec
# Is the cpu controller delegated into the slice? (must contain `cpu`)
cat /sys/fs/cgroup/missions.slice/cgroup.subtree_control
# Which cgroup is a harness actually in? (claude/codex/opencode)
cat /proc/$(pgrep -n claude)/cgroup
```

Gotcha seen on prod 2026-06-15: every slice sat at the default `cpu.weight=100`
with `CPUQuota=infinity`, the `cpu` controller was **not** delegated into
`missions.slice` (`subtree_control = memory pids`), and the harness CLIs live
*inside* `missions.slice` next to the build hogs. Boosting only the API service
(`CPUWeight=10000`) didn't help because the bottleneck was the harness, not the
API. A single mission running ~21 parallel `harness.cli run-task` jobs
oversubscribed all 8 cores.

### Fix — aggregate slice guard (persistent drop-in)

```bash
mkdir -p /etc/systemd/system/missions.slice.d
cat > /etc/systemd/system/missions.slice.d/cpu.conf <<'EOF'
[Slice]
# Missions yield CPU to the API tier (system.slice = 100) under contention,
# and can never take more than ~6.5 of 8 cores in aggregate.
CPUWeight=30
CPUQuota=650%
EOF
systemctl daemon-reload
# Apply to the already-running slice immediately (runtime, also covered by the
# drop-in on next boot):
systemctl set-property missions.slice CPUWeight=30 CPUQuota=650%
```

Tune `CPUQuota` to `(cores - ~1.5) * 100%`. A runtime-only `set-property`
without the drop-in is lost on restart — always write the drop-in too.

### Fix — per-mission guard (code, env-driven)

Set `MISSION_CPU_WEIGHT` (low, e.g. 40) and `MISSION_CPU_QUOTA` (e.g. `400%`) in
the prod env file so each new mission scope is capped; existing missions can be
retuned live from the dashboard **Resources** panel (or
`POST /api/workspaces/:id/resources` with `cpu_weight`/`cpu_quota`). Setting any
CPU property is also what makes systemd delegate the `cpu` controller into
`missions.slice` — without it, a per-scope `CPUQuota` is silently unenforced.
See the "Resource isolation" section in `CLAUDE.md` for the architecture.
