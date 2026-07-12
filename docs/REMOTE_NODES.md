# Remote Runner Nodes MVP

Remote nodes are an MVP execution path for selected missions. Core remains the
source of truth for dashboard auth, mission records, events, and UI state. A
node receives only a per-mission lease token signed from its own node secret.
It never receives a dashboard JWT or broad API token.

## Current Scope

Supported now:

- core lists configured nodes and reports cached fleet status (heartbeat v2:
  capacity, labels, CPU/memory/disk figures, job counts)
- a background fleet monitor polls every node's `/heartbeat` on an interval
  (`REMOTE_NODE_MONITOR_SECS`, default 15s, `0` disables) and derives per-node
  statuses: `online` (fresh heartbeat), `degraded` (1-2 consecutive misses),
  `offline` (3+ consecutive misses)
- `sandboxed-node` exposes authenticated `/heartbeat` and `/execute`
- node bearer-token rotation: the node accepts the current token and, during a
  rotation window, the previous one (`SANDBOXED_NODE_TOKEN_PREVIOUS`)
- selected missions can run one `remote_command` on a selected node
- the command runs under `SANDBOXED_NODE_WORK_DIR/<mission-id>`
- dispatch failures fail closed: the mission is marked failed and the API
  returns an error
- future `not_before` scheduling with `remote_node_id` is rejected for now so
  remote commands are never started before their requested dispatch window

Not supported yet:

- full AI backend execution on remote nodes
- live token/tool streaming from remote back to core
- workspace/container sync between core and node
- node-side access to dashboard auth, mission DB, or broad core APIs
- scheduled remote dispatch after a future `not_before`

## Build

On core and each node:

```bash
cargo build --bin sandboxed-sh --bin sandboxed-node
```

Install the node binary on each runner:

```bash
sudo install -m 0755 target/debug/sandboxed-node /usr/local/bin/sandboxed-node
```

## Node Configuration

Use one distinct secret per node. Do not paste the secret into logs or docs.

The node binds to `127.0.0.1:3088` by default so it is never reachable over a
network by accident. To let core reach it, set `SANDBOXED_NODE_BIND` to a
private interface — preferably the node's tailscale IP (e.g.
`SANDBOXED_NODE_BIND=100.77.4.93:3088`), or `0.0.0.0:3088` only when the host
firewall restricts the port to core.

Example (Babylon):

```bash
export SANDBOXED_NODE_ID=babylon
# Bind a private/tailscale interface; default is 127.0.0.1:3088.
export SANDBOXED_NODE_BIND=<tailscale-ip>:3088
export SANDBOXED_NODE_WORK_DIR=/var/lib/sandboxed-node/work
export SANDBOXED_NODE_CAPACITY=1
export SANDBOXED_NODE_TOKEN="$SANDBOXED_REMOTE_NODE_BABYLON_TOKEN"
# Optional: comma-separated capability labels reported in heartbeats.
export SANDBOXED_NODE_LABELS=lean,docker
/usr/local/bin/sandboxed-node
```

Nippur and Ashur are configured identically with their own
`SANDBOXED_NODE_ID` and token.

### Token rotation

To rotate a node token with zero downtime:

1. On the node, set `SANDBOXED_NODE_TOKEN` to the new secret and
   `SANDBOXED_NODE_TOKEN_PREVIOUS` to the old one, then restart the node. It
   now accepts both (constant-time comparison for each).
2. Update the core env (`SANDBOXED_REMOTE_NODE_<ID>_TOKEN`) to the new secret
   and restart/redeploy core.
3. Remove `SANDBOXED_NODE_TOKEN_PREVIOUS` on the node and restart it.

### Heartbeat v2

`GET /heartbeat` returns the v1 fields (`node_id`, `online`, `capacity_total`,
`capacity_available`, `active_leases`, `version`) plus:

- `protocol_version` (currently `2`; core treats a missing field as `1`)
- `labels` (from `SANDBOXED_NODE_LABELS`)
- `cpu_total` (logical cores)
- `mem_total_bytes` / `mem_available_bytes`
- `disk_total_bytes` / `disk_available_bytes` (filesystem backing the work
  dir, falling back to root)
- `active_jobs` / `queued_jobs` (async job API)
- `cached_toolchains` (empty until toolchain prewarming ships)

All new fields are serde-default-tolerant in both directions, so mixed-version
core/node fleets keep working during upgrades.

Minimal systemd unit:

```ini
[Unit]
Description=sandboxed.sh remote runner node
After=network-online.target docker.service
Wants=network-online.target

[Service]
EnvironmentFile=/etc/sandboxed-node.env
ExecStart=/usr/local/bin/sandboxed-node
Restart=always
RestartSec=5
User=root
WorkingDirectory=/var/lib/sandboxed-node

[Install]
WantedBy=multi-user.target
```

`/etc/sandboxed-node.env` contains only env-var assignments such as
`SANDBOXED_NODE_ID`, `SANDBOXED_NODE_BIND`, `SANDBOXED_NODE_WORK_DIR`,
`SANDBOXED_NODE_CAPACITY`, `SANDBOXED_NODE_TOKEN`,
`SANDBOXED_NODE_TOKEN_PREVIOUS` (rotation only), and
`SANDBOXED_NODE_LABELS`.

## Core Configuration

Configure the three Thomas/Paloma servers on the core backend:

```bash
export SANDBOXED_REMOTE_NODES_ENABLED=true
export SANDBOXED_REMOTE_NODES='babylon=http://54.36.175.109:3088,nippur=http://37.187.92.183:3088,ashur=http://188.40.69.160:3088'
export SANDBOXED_REMOTE_NODE_BABYLON_TOKEN='<set in environment only>'
export SANDBOXED_REMOTE_NODE_NIPPUR_TOKEN='<set in environment only>'
export SANDBOXED_REMOTE_NODE_ASHUR_TOKEN='<set in environment only>'
```

The token env names are also the default names inferred from the node ids. To
override a token env name, use `id=url|TOKEN_ENV_NAME` in
`SANDBOXED_REMOTE_NODES`.

Optional: `REMOTE_NODE_MONITOR_SECS` sets the fleet-monitor poll interval
(default 15; `0` disables the loop, in which case `/api/remote-nodes` probes
uncached nodes on demand).

## Smoke Tests

Check fleet status from core:

```bash
curl -H "Authorization: Bearer $DASHBOARD_JWT" \
  "$SANDBOXED_CORE_URL/api/remote-nodes"
```

The response is `{ "enabled": bool, "nodes": [...], "recent_jobs": [...] }`.
Each node entry carries the cached `status`
(`online`/`degraded`/`offline`/`unknown`), labels, capacity, job counts,
memory/disk availability, `cached_toolchains`, and `last_seen`; `recent_jobs`
lists the last dispatch outcomes across the fleet.

Run a selected remote MVP mission:

```bash
curl -sS -X POST "$SANDBOXED_CORE_URL/api/control/missions" \
  -H "Authorization: Bearer $DASHBOARD_JWT" \
  -H "Content-Type: application/json" \
  -d '{
    "title": "remote smoke on babylon",
    "backend": "codex",
    "remote_node_id": "babylon",
    "remote_command": "hostname && docker --version && pwd && echo sandboxed-node-ok"
  }'
```

Repeat with `"nippur"` and `"ashur"` for those nodes. The mission is stored in
core. Its final assistant event contains the remote command stdout/stderr and
the status becomes `completed` only when the remote command exits with code 0.

## Failure Behavior

- Unknown selected node: request fails with a clear error.
- Remote nodes disabled: request fails with a clear error.
- Missing node token env: mission is marked `failed`; the error names only the
  missing env var.
- Node offline or rejects lease: mission is marked `failed`; no local success is
  reported.
- Non-zero command exit: mission is stored and marked `failed` with the remote
  stdout/stderr event preserved.

## Production Readiness Gaps

Before production deploy, this needs remote workspace synchronization, remote
AI backend process supervision, streamed event forwarding, durable node lease
tracking, capacity-aware queueing, TLS or private-network enforcement, and
operator UI for selecting nodes.
