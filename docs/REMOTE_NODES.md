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
- async job API on the node (`POST /jobs`, `GET /jobs/:id`,
  `POST /jobs/:id/cancel`, `GET /jobs`): jobs are persisted to
  `<workdir>/jobs.db`, run under the node-wide capacity semaphore shared with
  `/execute` (`SANDBOXED_NODE_CAPACITY`), capture combined stdout+stderr to
  `<workdir>/logs/<job-id>.log`, and are killed by process group on
  cancel/timeout (`SANDBOXED_NODE_MAX_JOB_SECS`, default 14400s). Jobs left
  in flight across a node restart are marked `lost`
- non-blocking core dispatch: pass `"remote_async": true` alongside
  `remote_node_id`/`remote_command` in `POST /api/control/missions` — the
  mission goes Active immediately and a background poll loop (3s) finalizes
  it when the job completes; 5 consecutive unreachable polls fail the mission
  with reason `remote_node_lost`
- declarative `lean_build` jobs: the node fetches a pinned commit itself,
  runs a constrained `lake`/`lean`/`elan` argv with shared elan/lake caches,
  and reports artifact digests (see "Lean build jobs" below)
- capacity-aware auto placement: `remote_node_id: "auto"` (plus optional
  `remote_requirements` labels) picks the least-loaded eligible node from the
  fleet cache and fails closed with a per-node exclusion report
- `POST /api/remote-build`: an in-workspace, capability-token-authenticated
  endpoint that dispatches a `lean_build` job (auto-placed by default) and
  waits for the result — used by the `remote-lean-build` wrapper
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

`SANDBOXED_NODE_CAPACITY`, when set, must be a positive integer. It is one
node-wide execution budget: synchronous `/execute` leases and asynchronous
jobs consume the same permits. A full node rejects a new `/execute` request
with HTTP 429; queued jobs wait for a shared permit. This prevents the two API
paths from each consuming the configured capacity independently.

A node configured with the `lean` label must have an executable `lake` proxy
either at `$SANDBOXED_NODE_WORK_DIR/caches/elan/bin/lake` or on the service's
absolute `PATH`. Install Elan under the `sandboxed-node` account and prewarm the
project toolchains (for example `leanprover/lean4:v4.24.0`) before adding the
label. The heartbeat reports `lean_runtime_ready`; when the proxy is missing,
the node withholds `lean` and core rejects it for Lean placement instead of
accepting jobs that will fail with `ENOENT`.

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
- `cached_toolchains` (Lean toolchain dirs under the node's shared elan
  cache; empty until the first `lean_build` job warms it)
- `lean_runtime_ready` (`true` when the Lake proxy used by declarative builds
  is executable; older nodes deserialize this as unknown)

`capacity_available` is a snapshot of the shared semaphore, so it already
accounts for work admitted through either `/execute` or `/jobs`.

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
# Replace 999 with `id -u sandboxed-node` on this host. System-unit `%U`
# expands to the system manager (root), not to the account named by `User=`.
Environment=XDG_RUNTIME_DIR=/run/user/999
Environment=DBUS_SESSION_BUS_ADDRESS=unix:path=/run/user/999/bus
ExecStart=/usr/local/bin/sandboxed-node
Restart=always
RestartSec=5
User=sandboxed-node
Group=sandboxed-node
WorkingDirectory=/var/lib/sandboxed-node
NoNewPrivileges=true
PrivateTmp=true
# Hide every host home and runtime directory, then expose only the runner's
# user-manager runtime. Replace 999 with `id -u sandboxed-node` here too.
ProtectHome=tmpfs
BindReadOnlyPaths=/run/user/999
ProtectSystem=strict
ReadWritePaths=/var/lib/sandboxed-node
ProtectKernelTunables=true
ProtectKernelModules=true
ProtectControlGroups=true

[Install]
WantedBy=multi-user.target
```

Create the service account and writable state directory before starting the
unit (`useradd --system --home /var/lib/sandboxed-node sandboxed-node` and
`install -d -o sandboxed-node -g sandboxed-node /var/lib/sandboxed-node`).
Enable its user manager once with
`loginctl enable-linger sandboxed-node`; job commands then run in transient
user scopes, so even descendants that call `setsid` are reaped when their job
finishes. If the user bus is unavailable, the runner safely falls back to
process-group cleanup and logs any failed scope stop.

The runner derives `/run/user/<effective-uid>` itself. Do not put
`XDG_RUNTIME_DIR=/run/user/%U` in a system unit: `%U` describes the systemd
manager user there (normally root), not the account named by `User=`. Use the
numeric result of `id -u sandboxed-node` for both explicit paths in the unit.
A stale or inaccessible configured value is ignored in favour of the
effective-UID path, but the matching `BindReadOnlyPaths` is still required when
`ProtectHome=tmpfs` hides `/run/user`.
Lean/Lake builds execute repository-controlled code, so running this service as
root is unsupported; the argv allowlist is defense in depth, not a sandbox.
The binary refuses UID 0 by default; `SANDBOXED_NODE_ALLOW_ROOT=1` is available
only as an explicit emergency migration override.

`/etc/sandboxed-node.env` contains only env-var assignments such as
`SANDBOXED_NODE_ID`, `SANDBOXED_NODE_BIND`, `SANDBOXED_NODE_WORK_DIR`,
`SANDBOXED_NODE_CAPACITY`, `SANDBOXED_NODE_TOKEN`,
`SANDBOXED_NODE_TOKEN_PREVIOUS` (rotation only), `SANDBOXED_NODE_LABELS`,
`SANDBOXED_NODE_MAX_JOB_SECS`, and `SANDBOXED_NODE_MAX_QUEUED` (jobs waiting
behind capacity; defaults to four times `SANDBOXED_NODE_CAPACITY`).

Node env vars for lean-build jobs:

- `SANDBOXED_NODE_ENV_ALLOWLIST` — comma-separated env keys a `lean_build`
  payload may set (default `LEAN_NUM_THREADS,LAKE_JOBS`); anything else is
  rejected before the build starts.
- `SANDBOXED_NODE_MIN_FREE_GB` — free-space floor (default 10 GiB) for the
  node's cache GC: every 30 minutes, when the work-dir filesystem drops below
  it, checkout dirs then lake cache slots are LRU-deleted (by dir mtime)
  until the threshold is met.
- `SANDBOXED_NODE_GIT_SSH_KEY` — path to an SSH key used for git fetches
  (`GIT_SSH_COMMAND="ssh -i <key> -o IdentitiesOnly=yes -o
  StrictHostKeyChecking=accept-new"`); unset = default git auth.

## Async Job API

The node exposes a durable job API next to the blocking `/execute` path. All
endpoints require the shared bearer token; `POST /jobs` additionally requires
a per-job HMAC lease scoped to `job:submit` (minted by core; `mission:execute`
leases are rejected, and vice versa).

- `POST /jobs` with `{job_id, mission_id, lease_token, payload}` where
  `payload` is `{"kind": "raw_command", "command": "...", "timeout_secs"?:
  N, "env"?: {..}}` or a `lean_build` payload (below). Returns `202 {job_id,
  state: "queued"}`; duplicate `job_id` returns `409`.
- `GET /jobs/:id` — full status (`queued|running|succeeded|failed|cancelled|
  lost`, exit code, timestamps, error) plus up to the last 64 KiB of the
  combined log as `log_tail` and, for successful builds, `artifacts`
  (`[{path, sha256, size_bytes}]`).
- `POST /jobs/:id/cancel` — SIGTERMs the job's process group (SIGKILL after
  10s); returns the current state and whether a live job got the request.
- `GET /jobs?limit=N` — recent jobs, newest first (default 20, no log tails).

Async mission dispatch from core:

```bash
curl -sS -X POST "$SANDBOXED_CORE_URL/api/control/missions" \
  -H "Authorization: Bearer $DASHBOARD_JWT" \
  -H "Content-Type: application/json" \
  -d '{
    "title": "async remote smoke on babylon",
    "remote_node_id": "babylon",
    "remote_command": "hostname && sleep 30 && echo done",
    "remote_async": true
  }'
```

The API returns immediately with the mission Active; core polls the job every
3s, logs a progress note on job state changes, and finalizes the mission with
reason `remote_node_job` (or `remote_node_lost` when the node stays
unreachable for 5 consecutive polls). Cancelling from the dashboard works
indirectly: when the mission leaves the Active state for any reason, the poll
loop cancels the node job on its next tick. There is no dedicated
mission-cancel -> job-cancel plumbing yet; wiring an explicit cancel hook is a
follow-up.

## Lean Build Jobs

`lean_build` is a declarative job payload: no workspace sync, no shell. The
node checks out the pinned commit itself and runs a constrained argv:

```json
{
  "kind": "lean_build",
  "source": {"repo": "https://github.com/org/repo.git", "commit": "<40-hex sha>"},
  "cwd_rel": "morpho-verity",
  "command": ["lake", "build"],
  "timeout_secs": 3600,
  "cache_key": null,
  "artifacts": [".lake/build/lib/*"],
  "env": {"LEAN_NUM_THREADS": "8"}
}
```

Validation (node-side, before anything runs):

- `source.commit` must be a full 40-char lowercase hex SHA (no branches).
- `cwd_rel` uses the same strict path allowlist as the Spark offload
  (`[A-Za-z0-9._-]` components, no `..`, no `-`-leading component).
- `command` is executed directly (never via a shell); accepted entry points are
  `lake build ...` and `lean ...`. The service environment is cleared before
  execution so node bearer/signing secrets are not inherited by build code.
- `env` keys must be within `SANDBOXED_NODE_ENV_ALLOWLIST`.
- When the payload omits `LEAN_NUM_THREADS` or `LAKE_JOBS`, the node divides
  its usable logical-CPU budget (`available_parallelism`, including OS
  affinity/cgroup caps) across both levels. Lake defaults to at most four jobs
  and Lean receives the remaining per-job thread budget. When only one key is
  supplied, the other is derived from the remaining CPU budget. Explicit
  allowlisted payload values still take precedence. Direct `lean ...` jobs do
  not have Lake fan-out, so they receive the full thread budget and default
  `LAKE_JOBS` to one.
- `timeout_secs` is clamped to `SANDBOXED_NODE_MAX_JOB_SECS`.

Execution model:

- Checkouts are content-addressed at
  `<workdir>/checkouts/<sha256(repo)[..16]>/<commit>/` and reused across
  jobs; materialization is `git init` + `git fetch --depth 1 <repo>
  <commit>` + detached checkout (+ best-effort shallow submodules) into a
  temp dir, then an atomic rename. Builds of the same commit are serialized
  by a file lock.
- Builds run with shared caches: `ELAN_HOME=<workdir>/caches/elan`,
  `XDG_CACHE_HOME=<workdir>/caches/xdg`, `HOME=<workdir>/caches/home`, and
  `<workdir>/caches/elan/bin` prepended to `PATH`.
- Lake cache: `cache_key` defaults to a digest of the build cwd's
  `lean-toolchain` + `lake-manifest.json`. Before a cold build the slot
  `<workdir>/caches/lake/<key>/` is hardlink-copied (`cp -al`) into
  `<cwd>/.lake`; after a successful build the slot is refreshed the same way
  (tmp dir + atomic rename, under a per-key flock). Accepted caveat: mtime
  drift may cause partial rebuilds, never wrong artifacts.
- `artifacts` patterns are resolved relative to the checkout root after a
  successful build: exact relative paths, or `*` within a single path
  segment (never across `/`); `..` and absolute paths are rejected. Each
  match is recorded with its sha256 and size.
- The heartbeat's `cached_toolchains` lists the directory names under
  `<workdir>/caches/elan/toolchains/`.

## Auto Placement

`POST /api/control/missions` accepts `remote_node_id: "auto"` (optionally
with `remote_requirements: ["lean", ...]`, default none for raw commands),
and `POST /api/remote-build` defaults to it (requirements default
`["lean"]`). Placement runs against the fleet monitor's cached heartbeats
**before** the mission/job is persisted, so failures surface as a clean API
error.

A node is eligible when all of the following hold:

- cached status is `online`
- its labels (heartbeat, falling back to static config) cover every
  requirement
- `disk_available` ≥ `REMOTE_NODE_MIN_DISK_GB` (default 20)
- `mem_available` ≥ `REMOTE_NODE_MIN_MEM_GB` (default 8)
- `active_jobs + queued_jobs + active_leases + core reservations < 2 * capacity_total`

Eligible nodes are ranked least-loaded first (ties broken by most available
memory). When no node qualifies the request **fails closed** with every
configured node listed alongside its exclusion reason (offline / missing
label X / low disk / low memory / busy), e.g.
`no eligible remote node (babylon: low disk (12 GiB available, 20 GiB
required); nippur: missing label 'lean')`.

## POST /api/remote-build

Dispatches a `lean_build` job from inside a mission workspace. Auth is a
per-mission HMAC capability token (same signing secret as the spark-offload
token, domain-separated with a `remote-build:` prefix), injected into
harness envs as `REMOTE_BUILD_URL` / `REMOTE_BUILD_TOKEN` /
`REMOTE_BUILD_MISSION_ID` whenever remote nodes (or spark offload) are
enabled. Node bearer tokens never enter the workspace.

Mission preparation also installs `remote-lean-build` and exports its exact
path as `REMOTE_BUILD_COMMAND`. Container missions receive it in
`/usr/local/bin`; host missions receive an isolated copy under the mission
directory, which is prepended to `PATH`.

Request body:

```json
{
  "mission_id": "<uuid>",
  "token": "<REMOTE_BUILD_TOKEN>",
  "repo": "https://github.com/org/repo.git",
  "commit": "<40-hex sha>",
  "cwd_rel": "verity",
  "command": ["lake", "build"],
  "timeout_secs": 3600,
  "requirements": ["lean"],
  "node_id": "auto",
  "wait": true,
  "artifacts": [".lake/build/lib/*"]
}
```

`requirements` defaults to `["lean"]`, `node_id` to `"auto"`, `wait` to
`true`. With `wait: true` the call polls the node every 3s (client-side cap
2h) and returns `{exit_code, state, duration_secs, log_tail, node_id,
job_id, artifacts}`. With `wait: false` it returns `202 {job_id, node_id}`;
poll `GET /api/remote-build/:job_id?mission_id=...&node_id=...` with the
capability in `Authorization: Bearer $REMOTE_BUILD_TOKEN` for the job status.
Capabilities expire after six hours by default (`REMOTE_BUILD_TOKEN_TTL_SECS`)
and new submissions require a live mission. Placement failures and
unconfigured/unavailable fleets
answer `503` with the reason, so callers can fall back to a local build.

## remote-lean-build wrapper

`scripts/remote-lean-build` is the in-workspace client for the endpoint. Run
it from anywhere inside a git checkout:

```bash
remote-lean-build                # lake build at your current repo position
remote-lean-build lake build Verity
```

It derives `repo` (`git remote get-url origin`), `commit`
(`git rev-parse HEAD`) and `cwd_rel` (`git rev-parse --show-prefix`),
refuses a dirty tree (exit 2 — the node builds a pinned commit, so
uncommitted changes would be silently ignored), POSTs to
`$REMOTE_BUILD_URL` with `$REMOTE_BUILD_TOKEN`/`$REMOTE_BUILD_MISSION_ID`,
prints the remote log tail, and exits with the remote build's exit code. On
HTTP 503 (placement failure, fleet down, feature off) it prints the reason
and exits 75 (`EX_TEMPFAIL`) so scripts can fall back to a local build:

```bash
remote-lean-build || { [ $? -eq 75 ] && lake build; }
```

Optional: `REMOTE_BUILD_TIMEOUT_SECS` forwards a job timeout (clamped by the
node's `SANDBOXED_NODE_MAX_JOB_SECS`).

Workspace configuration can pin the placement policy without exposing node
credentials:

```json
{
  "remote_build": {
    "node_id": "auto",
    "requirements": ["lean"],
    "timeout_secs": 3600
  }
}
```

Auto placement combines heartbeat load with durable core-side reservations,
and placement plus reservation is serialized. Concurrent requests therefore
spread across eligible runners before their next heartbeat instead of all
selecting the same apparently idle node.

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
