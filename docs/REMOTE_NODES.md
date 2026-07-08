# Remote Runner Nodes MVP

Remote nodes are an MVP execution path for selected missions. Core remains the
source of truth for dashboard auth, mission records, events, and UI state. A
node receives only a per-mission lease token signed from its own node secret.
It never receives a dashboard JWT or broad API token.

## Current Scope

Supported now:

- core lists configured nodes and reports live heartbeat/capacity status
- `sandboxed-node` exposes authenticated `/heartbeat` and `/execute`
- selected missions can run one `remote_command` on a selected node
- the command runs under `SANDBOXED_NODE_WORK_DIR/<mission-id>`
- dispatch failures fail closed: the mission is marked failed and the API
  returns an error

Not supported yet:

- full AI backend execution on remote nodes
- live token/tool streaming from remote back to core
- workspace/container sync between core and node
- node-side access to dashboard auth, mission DB, or broad core APIs

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

Babylon:

```bash
export SANDBOXED_NODE_ID=babylon
export SANDBOXED_NODE_BIND=0.0.0.0:3088
export SANDBOXED_NODE_WORK_DIR=/var/lib/sandboxed-node/work
export SANDBOXED_NODE_CAPACITY=1
export SANDBOXED_NODE_TOKEN="$SANDBOXED_REMOTE_NODE_BABYLON_TOKEN"
/usr/local/bin/sandboxed-node
```

Nippur:

```bash
export SANDBOXED_NODE_ID=nippur
export SANDBOXED_NODE_BIND=0.0.0.0:3088
export SANDBOXED_NODE_WORK_DIR=/var/lib/sandboxed-node/work
export SANDBOXED_NODE_CAPACITY=1
export SANDBOXED_NODE_TOKEN="$SANDBOXED_REMOTE_NODE_NIPPUR_TOKEN"
/usr/local/bin/sandboxed-node
```

Ashur:

```bash
export SANDBOXED_NODE_ID=ashur
export SANDBOXED_NODE_BIND=0.0.0.0:3088
export SANDBOXED_NODE_WORK_DIR=/var/lib/sandboxed-node/work
export SANDBOXED_NODE_CAPACITY=1
export SANDBOXED_NODE_TOKEN="$SANDBOXED_REMOTE_NODE_ASHUR_TOKEN"
/usr/local/bin/sandboxed-node
```

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
`SANDBOXED_NODE_CAPACITY`, and `SANDBOXED_NODE_TOKEN`.

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

## Smoke Tests

Check live node status from core:

```bash
curl -H "Authorization: Bearer $DASHBOARD_JWT" \
  "$SANDBOXED_CORE_URL/api/remote-nodes"
```

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
