# Remote Runner Nodes MVP

Remote nodes are an MVP execution path for selected missions. Core remains the
source of truth for dashboard auth, mission records, events, and UI state. A
node receives only a per-mission lease token signed from its own node secret.
It never receives a dashboard JWT or broad API token.

## Current Scope

Supported now:

- core lists configured nodes and reports cached fleet status (heartbeat v3:
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
  endpoint that dispatches a `lean_build` job (auto-placed by default). The
  wrapper submits asynchronously and polls the mission-scoped status endpoint
  so interrupted clients can resume the same immutable job.
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
paths from each consuming the configured capacity independently. Lean jobs
also divide the process's usable CPU affinity by this capacity before deriving
their default Lake/Lean fan-out, so two admitted jobs do not each claim the
whole node. Set capacity to the number of builds the host can sustain; explicit
payload concurrency overrides remain an operator escape hatch.

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

### Heartbeat v3

`GET /heartbeat` returns the v1 fields (`node_id`, `online`, `capacity_total`,
`capacity_available`, `active_leases`, `version`) plus:

- `protocol_version` (currently `4`; core treats a missing field as `1`).
- `source_bundle_capacity`: `{ "overlay_bytes": 1048576, "complete_bytes": 33554432 }`
  by default. These are effective decoded file-byte ceilings; positive
  `SANDBOXED_NODE_MAX_SOURCE_BUNDLE_BYTES` overrides apply to both values.
  The fleet API and `get_compute_fleet` expose this capability; legacy nodes
  report it as absent/unknown.
  Version 3 is required for overlay jobs and version 4 for complete-source
  jobs, so a rolling deployment can never silently fetch a private repository
  or drop source content on an older node.
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

New fields are serde-default-tolerant in both directions. Features whose
semantics an older node would silently ignore are additionally gated by the
reported protocol version.

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
  node's admission and cache GC. A build is rejected unless its declared
  scratch estimate fits above this floor. Every 30 minutes, when the work-dir
  filesystem drops below it, checkout dirs then lake cache slots are
  LRU-deleted (by dir mtime) until the threshold is met.
- `SANDBOXED_NODE_MAX_SOURCE_BUNDLE_BYTES` — maximum decoded size of a local
  source bundle (default 1 MiB/256 files for overlays and 32 MiB/4096 files
  for complete snapshots). An explicit positive byte override applies to both
  modes; it does not raise either file-count cap or any HTTP/JSON ceiling. Paths, per-file hashes and the manifest hash are
  verified before any file is applied.
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

### Typed remote launches (server-owned harness)

Clients that select a machine in a picker send the Orb shape instead of a
raw command:

```json
{"prompt": "...", "remote_node_id": "dgx-spark", "backend": "opencode",
 "model_override": "xai/grok-4.6", "project": "test",
 "idempotency_key": "<per-attempt key>"}
```

- The server plans the node execution from `backend` + `model_override`.
  Nodes run `claude` and `opencode` only (`REMOTE_NODE_HARNESSES`); selecting
  `grok`, `codex`, `gemini` or `chatgpt_ui` is a `400` naming the supported
  harnesses **before** any mission exists. Nothing is silently swapped.
- The harness talks back to this core's model proxy with a key minted for the
  mission (`remote-launch:<node>:<mission>`), delivered in the job env and
  never on the command line. Claude Code: `ANTHROPIC_BASE_URL` +
  `ANTHROPIC_AUTH_TOKEN`, `--model <bare id>`. OpenCode: the job env carries
  an inline config in `OPENCODE_CONFIG_CONTENT` (nothing is written into the
  cwd) declaring a `builtin` provider (`@ai-sdk/openai-compatible`,
  `baseURL <core>/v1`, `apiKey {env:SANDBOXED_PROXY_API_KEY}`) whose model
  map carries the exact requested id, and runs `--model builtin/<id>` — the
  same shape local OpenCode missions use, so the proxy receives e.g.
  `xai/grok-4.6` unchanged and applies its own chain/passthrough routing.
  The stock `openai` provider only lists OpenAI's catalog and would refuse
  such ids before any request. `model_override` is therefore required for
  OpenCode launches (`REMOTE_MODEL_REQUIRED` otherwise): nodes have no
  authenticated default model. A client-sent `builtin/` prefix is stripped
  so the map key and `--model` always agree. Prompts starting with `-` get a
  leading space so they stay positional (`--` is not safe on `opencode run`).
  The job's cwd is the node's per-mission directory (also its `HOME`), so
  nothing depends on the service user's home. `SANDBOXED_PUBLIC_URL` must be
  reachable from the node. Verified against the installed OpenCode 1.18 with
  a local mock `/v1/chat/completions`: the CLI resolves `builtin/xai/grok-4.6`
  and sends `model: "xai/grok-4.6"` with the env key as bearer (test
  `remote_opencode_execution_resolves_model_against_mock_proxy`).
- **Proxy key lifecycle.** The key is named `remote-launch:<mission id>`. It
  is deleted on every dispatch failure after minting, when the observer
  finishes (also after a restart re-attach), and at the reconciler's first
  pass for any key whose mission holds no ledger handle (a job on a node
  always has one: submission follows the tentative record). Nothing relies
  on the periodic `cleanup_keys`.
- `remote_command` still works verbatim (raw compatibility, own auth).
- A retry with the same `idempotency_key` (and project) coalesces onto the
  mission holding the dispatch key (`x-coalesced-with`) and never submits a
  second node job; a dispatch that already failed closed is not reused.
- The create response carries `execution` and `remote_job`; clients treat
  `remote_job.node_id`/`phase` as the authoritative placement.
- **Capability detection.** `GET /api/remote-nodes` carries
  `remote_launch: {typed: true, harnesses: ["claudecode","opencode"],
  raw_command: true, proxy_url_configured: bool, error_prefixes: [...]}`.
  The field is absent on older backends, which still require a raw
  `remote_command`; clients must keep their guard there.
- **Rejections before a mission exists** are plain-text `400` bodies with a
  stable prefix: `REMOTE_HARNESS_UNSUPPORTED: backend '<id>' cannot run on
  remote nodes: only claudecode or opencode are installed there …` and
  `REMOTE_PROMPT_REQUIRED: …`, `REMOTE_MODEL_REQUIRED: …` (OpenCode without
  `model_override`). Unknown node / disabled fleet keep their
  existing 400 text. Dispatch failures after creation (node rejected, token
  missing, `SANDBOXED_PUBLIC_URL` unset) are `502` with the mission marked
  `failed`/`remote_dispatch_failed` and its prompt persisted.
- **Success response** (`200`) is the mission JSON plus `execution`
  (`{state: "waiting_remote_job", run_id, generation, heartbeat_at, scope_unit:
  "remote-node:<id>", …}`) and `remote_job`
  (`{job_id, node_id, phase: "observed", node_state: "queued"|"running"|…,
  accepted_at, heartbeat_at, observed_age_secs, …}`). `remote_job.node_state`
  is the node's last reported job state (queued until the node starts it);
  `phase` is core's observation health. A coalesced retry answers `200` with
  header `x-coalesced-with: <mission id>` and the same shape.
- `scripts/remote-launch-canary.sh` exercises this shape against a real node.

### Raw remote mission lifecycle (durable ownership)

A raw remote mission never starts a local harness, so its liveness cannot be
read from the runner list. The create request therefore establishes durable
ownership before it responds, and every supervisor consults that ownership
instead of treating the mission as an orphan (incident ab1792b4, 2026-09-20:
the stuck-mission watchdog interrupted a freshly accepted dgx-spark mission
seven seconds after dispatch with `orphan_no_runner`, the poll loop read that
as an operator cancellation and cancelled the node job, and the mission kept
no prompt, no run and no record of the outcome).

- **Prompt.** `prompt` is persisted as a real `user_message` event before the
  job is submitted and before the response. It appears in `history`,
  `get_initial_user_message` and event replay even when dispatch fails.
- **Run lease.** After node acceptance and the ledger handle, core acquires a
  mission run lease owned by `remote-job:<job_id>` with scope
  `remote-node:<node_id>` in state `waiting_remote_job`, then marks the
  mission Active. The poll loop refreshes the ledger heartbeat and the lease on
  every successful observation and finishes the lease with the terminal
  reason. `GET /api/control/missions/:id` exposes it as `execution`.
- **Placement read model.** Mission reads carry a `remote_job` block:
  `job_id`, `node_id`, `phase` (`observed`, `unobserved`, `submit_ambiguous`,
  `finished`, `lease_only`), the last node-reported `node_state`/`exit_code`
  when known, `accepted_at`/`heartbeat_at`/`observed_age_secs` from the ledger
  and `terminal_reason` from the settled lease. `null` means the mission never
  dispatched a raw remote job. The row's `workspace`/`backend` still describe
  the local harness a resume would run, not the remote placement.
- **Watchdog.** An Active mission with no runner is left alone while its
  accepted `mission` ledger handle (or a lease owned by the same job) was
  observed within `REMOTE_JOB_UNOBSERVED_SECS` (300 s). Past that window the
  mission is interrupted with reason `remote_job_unobserved` and the handle is
  retained as a cancellation fence; a mission with no accepted handle keeps
  the ordinary `orphan_no_runner` repair. An unreadable ledger defers the
  decision. Remote *build* leases (`remote-build:*`) keep their existing
  protection; a `remote-job:*` lease without a ledger handle is not liveness.
- **Restart.** Startup recovery skips missions that hold an accepted
  `mission` handle instead of marking them `server_shutdown`; the remote job
  reconciler re-attaches the poll loop, which rebuilds the lease. If the
  reconciler cannot re-attach (node config or token missing) the watchdog's
  unobserved rule eventually surfaces it instead of leaving it Active forever.
- **Interrupted before terminal.** When the job reaches a terminal state after
  the mission already left Active, the status is preserved but a durable
  assistant note records the node verdict and log tail, and the lease is
  finished with `remote_job_<state>`.

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
- When the payload omits `LEAN_NUM_THREADS` or `LAKE_JOBS`, the node first
  divides its usable logical-CPU budget (`available_parallelism`, including OS
  affinity/cgroup caps) by `SANDBOXED_NODE_CAPACITY`, then divides that per-job
  share across both levels. Lake defaults to at most four jobs and Lean
  receives the remaining per-job thread budget. When only one key is supplied,
  the other is derived from the remaining CPU budget. Explicit allowlisted
  payload values still take precedence. Direct `lean ...` jobs do not have Lake
  fan-out, so they receive the full per-job thread share and default
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
- Lake dependency cache: the copy/sync machinery is isolated, namespaced, and
  lock-protected, but restore and sync currently fail closed. Lake 4.24 does
  not persist an authoritative evaluated root `buildDir` in
  `lake-manifest.json`; checked-in manifest fields can also be stale or disagree
  with a dynamic lakefile. The node therefore never trusts manifest-only layout
  assertions and keeps repository `.lake` trees cold until a trusted Lake
  configuration query is implemented. Shared Elan/XDG/Home caches remain
  enabled, so toolchains and downloads are still reused without sharing root
  build artifacts across commits.
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
- effective disk after non-terminal reservations is at least the greater of
  `REMOTE_NODE_MIN_DISK_GB` (default 20) and the request's scratch estimate
  plus `REMOTE_NODE_DISK_EMERGENCY_GB` (default 10)
- `mem_available` ≥ `REMOTE_NODE_MIN_MEM_GB` (default 8)
- `active_jobs + queued_jobs + active_leases + core reservations < 2 * capacity_total`

Eligible nodes are ranked by immediate capacity, specialization, and normalized
utilization. Ordinary CPU/Lean jobs prefer non-GPU nodes while those nodes have
an immediate slot, preserving GPU runners for inference. A free GPU node is
still selected before queueing behind a saturated CPU node, and an explicit
`"gpu"` requirement selects only GPU-capable nodes. Within a tier, lower
`(active + queued + leases + reservations) / capacity_total` wins, with
available memory and stable node id as tie-breakers. When no node qualifies the
request **fails closed** with every
configured node listed alongside its exclusion reason (offline / missing
label X / low disk after reservations / low memory / busy).

Hermes receives the same live capacity through the assistant MCP
`get_compute_fleet` tool. Controllers should call it before parallel dispatch,
then independently verify actual distribution from the returned remote job
receipts; declared capacity is not placement evidence.

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
  "expected_head": "<40-hex sha>",
  "toolchain": "leanprover/lean4:v4.19.0",
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
job_id, artifacts}`. With `wait: false` it returns `202` with the job/node plus
the credential-free immutable validation identity (repository, commit, command,
toolchain, and source-bundle digest). Poll
`GET /api/remote-build/:job_id?mission_id=...&node_id=...` with the capability
in `Authorization: Bearer $REMOTE_BUILD_TOKEN` for the job status. Pass
`expected_head=<live sha>` while polling to receive
`receipt_state=stale_success` and `current_head_match=false` when a green job
validated an older commit. Submission returns `409 STALE_VALIDATION_REQUEST`
when `expected_head` already differs from `commit`.
The core retains bounded terminal receipts in
`.sandboxed-sh/remote-job-receipts.json`, including the credential-free
repository, commit, command, toolchain, overlay digest, node, and job ID. This
keeps exact-head evidence available after the active reservation is released.
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
(`git rev-parse HEAD`), the Lean toolchain, and `cwd_rel`
(`git rev-parse --show-prefix`). Local tracked edits and allowlisted untracked
Lean/build metadata are sent as a bounded hashed overlay; tracked deletions and
renames use explicit delete/file operations. For a private repository, set
`REMOTE_BUILD_SOURCE_MODE=full`: the wrapper sends a content-addressed snapshot
of all tracked files (plus the same narrow untracked Lean/build metadata
allowlist), and the node materializes it without any Git fetch or repository
credential. The receipt binds both the declared commit and the snapshot digest.
Full mode recursively includes initialized Git submodules, including non-Lean
tracked files at every level. Each submodule's `HEAD` must match its parent's
pinned gitlink, and every repository's indexed gitlinks must match its pinned
tree. Initialize sources first
with `git submodule update --init --recursive` in an authorized source checkout;
missing initialization, wrong revisions, sparse missing files, unmerged index
entries, or uncommitted gitlink changes fail locally before submission with a
repair instruction. The wrapper does not fetch submodules or forward their Git
metadata or credentials.
Full mode ignores replacement refs and disables lazy fetching of missing Git
objects; materialize those objects in the authorized source checkout first.
Commit intended gitlink changes before submitting them. Ordinary local file
edits and the untracked Lean/build metadata allowlist apply recursively;
tracked file deletions are represented by omission. Files are globally sorted
and hashed using the existing complete-bundle and executable-operation digests.
The existing aggregate file/byte limits and path restrictions still apply to
the entire recursive snapshot; symlinks, including parent directories, are
rejected. Neither `.git` metadata nor `.lake` caches are transported.

This is source transport, not dependency download policy. Complete bundles
materialize without fetching the source repositories, but Lake/Elan may still
need permitted dependency/toolchain downloads or existing caches. This mode
does not supply runner credentials, transport dependency caches, or change
network policy. It uses the existing complete-bundle protocol (node v4 or newer).

Full snapshots default to **32 MiB decoded / 4096 files**, aggregated across
all recursive submodules. Overlays remain **1 MiB / 256 operations**.
`REMOTE_BUILD_MAX_SOURCE_BUNDLE_BYTES` and
`REMOTE_BUILD_MAX_SOURCE_BUNDLE_FILES` remain explicit sender overrides;
`SANDBOXED_NODE_MAX_SOURCE_BUNDLE_BYTES` remains the receiver byte override
for both modes. Existing smaller operator ceilings are respected. Raising a
sender limit alone cannot raise the receiver's fixed file-count cap or its
configured byte ceiling. No source or receipt should be removed to fit a cap.

The wrapper gzip-compresses its base64 JSON. Core ingress has a **50 MiB HTTP
body** cap and a separate **64 MiB decompressed JSON** cap, enforced while
reading at most cap + 1 bytes. Core forwards ordinary uncompressed
`SubmitJobRequest` JSON to the node; `/jobs` retains its **50 MiB HTTP body**
cap. These HTTP/JSON ceilings are fixed, independent of operator byte overrides.
32 MiB of decoded source occupies about 42.67 MiB in base64 before metadata;
the unchanged 50 MiB body limits leave room for the measured manifests and job
envelope. Path-heavy metadata or larger explicit overrides must still fit
all layers. Check the actual serialized request sizes; compression is not a
way to bypass decoded-source validation. Reverse proxies may impose smaller
body limits and must be verified on the intended route before rollout.

Roll out the backend (including its embedded sender and 64 MiB decoder),
refresh mission-managed wrappers through normal mission setup, and update an
idle node to the matching 32 MiB receiver before submitting larger snapshots.
A v4 heartbeat proves payload semantics. Updated nodes additionally advertise
`source_bundle_capacity`, computed by the same limit function as validation.
Before dispatch, core sums the actual decoded file bytes and filters automatic
placement (including re-probe selection) by the matching complete/overlay limit.
Explicit nodes use the same capacity check. Insufficient capacity returns 503
before a job is submitted, allowing the wrapper's existing local fallback;
400/422 submission failures are still caller errors and are not retried.
Decoded-byte capability is separate from wire capacity: after minting the lease,
core counts the exact compact JSON job envelope, including all metadata and
escaping, before recording a tentative handle or submitting. An envelope above
the unchanged 50 MiB node body cap returns 503 before dispatch even if an operator
has raised decoded-byte capacity (for example, 38 MiB of source exceeds 50 MiB
in base64 alone). This also protects metadata-heavy requests below the byte limit.
Legacy heartbeats remain eligible up to the historical 16 MiB complete / 1 MiB
overlay defaults, subject to the existing protocol gates. Larger payloads require
advertised capacity. Legacy private overrides cannot be inferred: upgrade nodes
with smaller overrides to advertise them before relying on automatic placement.
New nodes' explicit smaller ceilings are honored even below legacy defaults.
Old backends ignore the additive heartbeat field, so install the corrected
backend before enabling larger submissions; a node-only upgrade is insufficient.
Preserve explicit operator overrides,
active jobs and receipt/resume state. Require a >16 MiB complete-source canary
through the intended proxy/backend/node route, with source bytes, executable
bits, manifest/operations digests and terminal receipts checked. Existing
submodule initialization, path, symlink, replacement-ref and privacy checks
remain mandatory. This development change does not install, restart or deploy
production services. See [measured capacity evidence](REMOTE_SOURCE_CAPACITY.md).

It submits asynchronously to
`$REMOTE_BUILD_URL` with `$REMOTE_BUILD_TOKEN`/`$REMOTE_BUILD_MISSION_ID`,
persists the returned job/node receipt under
`${XDG_STATE_HOME:-$HOME/.local/state}/sandboxed-sh/remote-builds/`, then polls
the mission-authenticated status endpoint. Receipts are scoped to the remote
endpoint as well as the immutable request. Running the same command after a
client/tool interruption resumes the recorded job instead of submitting a
duplicate; if the terminal status was already persisted, it replays that
evidence without polling or resubmitting. Existing receipts remain resumable
when the checkout later becomes dirty. It prints the remote log tail and
exits with the remote build's exit code. Once a job has been accepted, polling
errors and client timeouts never request local fallback: rerun the identical
command to resume it. On
HTTP 503 (placement failure, fleet down, feature off) it prints the reason
and exits 75 (`EX_TEMPFAIL`) so scripts can fall back to a local build:

```bash
remote-lean-build || { [ $? -eq 75 ] && lake build; }
```

If the core cannot connect to the selected node before sending the request, it
returns 503 and local fallback remains safe. If submission instead has an
ambiguous response-side transport failure, core returns 502 while its tentative
ledger reconciles/cancels the possible job. The wrapper exits 1 for that
response and never falls back. It also persists a secret-free `ambiguous`
receipt, so an identical retry cannot submit a second job before operators
confirm reconciliation; `REMOTE_BUILD_FORCE_NEW=1` is an explicit override.

Optional: `REMOTE_BUILD_EXPECTED_HEAD` freezes an exact-head gate at
submission and is also sent on status polls. A `stale_success` receipt exits
non-zero even when the remote command itself exited zero.
`REMOTE_BUILD_TIMEOUT_SECS` forwards a job timeout (clamped by the
node's `SANDBOXED_NODE_MAX_JOB_SECS`). `REMOTE_BUILD_SUBMIT_TIMEOUT_SECS`
controls the pre-receipt HTTP budget (default 90 seconds, long enough for a
fresh probe plus node submission). Submit timeouts and response-side transport
failures are treated as ambiguous outcomes and never trigger local fallback;
only failures that happen before connecting are fallback-safe. Failure to
persist the receipt after a `202` is also fail-closed and reports the accepted
job handle. `REMOTE_BUILD_STATE_DIR` overrides the receipt directory. Receipts
never contain the capability token or repository credentials; they retain the
credential-free repository identity needed to audit the validation.
`REMOTE_BUILD_FORCE_NEW=1` deliberately bypasses a live matching receipt and
should be reserved for operator-directed retries.

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


### Content identity (plan step 6)

A build's identity is a versioned, canonical document hashed byte-for-byte
(`RemoteJobIdentity::identity_hash`): `base_tree_sha` (the root tree of the
commit; the commit itself is provenance only), `cwd_rel`, the exact `argv`,
sorted `artifacts`, `toolchain`, the overlay digest, `builder_image_digest`,
`build_protocol_version` and `behavior_env_digest` (an allowlisted,
credential-free environment digest computed by `remote-lean-build`). The
bundled wrapper sends `base_tree_sha`, `build_protocol_version` and
`behavior_env_digest`; the node verifies `HEAD^{tree}` against
`base_tree_sha` before building (`BASE_TREE_MISMATCH` otherwise).

Receipts written before identity v1 carry `version: 0`. They still block a
duplicate in-flight submission but are never replayed as success for a v1
request.

The JSON ledger (`remote-jobs.json`, `remote-job-receipts.json`) is mirrored
into `projects.db` (`remote_jobs`, `remote_job_subscribers`, `receipts`
kind=`build`) on every mutation; boot reconciliation backfills and logs a
parity report. The JSON files stay authoritative until one production
restart shows zero drift (plan step 9 retires them).

### Attach, durable subscribers, lineage (plan step 7)

- **One live job per identity, no 409.** A submission whose identity matches a
  job that is still `accepted`/`running` no longer fails with
  `REMOTE_VALIDATION_ALREADY_ACTIVE`. Core records the caller as a *subscriber*
  of the canonical job (`remote_job_subscribers`, JSON handle with the same
  `job_id`) and answers `202` with the canonical `job_id` and `"attached": true`.
  The canonical job runs once; every subscribed mission gets its own terminal
  receipt and its own wake, delivered exactly once per `(job_id, mission_id)`.
  `GET /api/remote-build/{job_id}?mission_id=…` accepts any subscribed mission.
- **Lineage guard.** `create_mission` refuses a child whose `parent_mission_id`
  is on the same project/track and is parked on a remote build:
  `409 {"error":"BUILD_IN_PROGRESS","job_id":…,"parent_mission_id":…}`. The
  parent is woken when the job ends; spawning a helper to poll it is never the
  answer. Lineage is the explicit parent id, never the origin session.
- **Passive harness.** `REMOTE_BUILD_PASSIVE=1` makes `remote-lean-build` exit
  `75` right after a durable submission instead of polling; re-running the exact
  command after the wake collects the receipt (or attaches if still running).
  The default poll interval for the non-passive path is now 15 s
  (`REMOTE_BUILD_POLL_SECS`).
<<<<<<< HEAD
=======

### DGX Spark in the fleet (plan step 8)

The Spark is the ordinary runner node `dgx-spark` (`sandboxed-node`, labels
`lean,gpu,high-memory`, capacity 1). What made it special — 128 GB of unified
memory shared with vLLM and a GitHub CI runner — is handled by an external
**slot provider** instead of a parallel job API:

- `SANDBOXED_NODE_SLOT_PROVIDER=arbiter`, `SANDBOXED_NODE_ARBITER_URL`,
  `SANDBOXED_NODE_ARBITER_TOKEN` (optional `SANDBOXED_NODE_ARBITER_PRIORITY`
  `P0|P1`, `SANDBOXED_NODE_ARBITER_MEM`). Before a job runs, the node asks
  `POST /slot/acquire {id, priority, mem}`; `503` means "draining, retry" and
  the node waits (5 s) until granted or cancelled. The slot is released on
  completion. An unreachable provider never lets a job through — that is the
  case the provider exists for.
- The arbiter persists granted slots (`~/.spark-arbiter/slots.json`), so a
  restart does not restart vLLM under a running build; slots nobody released
  expire after `ARBITER_SLOT_TTL_SEC` (6 h).
- `spark-build lake …`/`lean …` now execs `remote-lean-build` pinned to
  `dgx-spark` (override `SPARK_BUILD_NODE_ID`) with the same exit contract
  (91 = fleet unavailable). Other commands still use the legacy
  `/api/spark/offload` path until it is retired (step 9).
>>>>>>> 7d8a390e (Step 8: DGX Spark as a fleet node — arbiter slot provider on the node, spark-build via remote-lean-build)

### Native Grok goals (CLI 1.0.34)

A typed launch with `backend: "grok"`, `model_override: "grok-4.6"`,
`remote_node_id`, and a prompt beginning `/goal ` runs the native Grok CLI.
The command uses `--cwd "$PWD" --always-approve --no-plan --output-format
streaming-json -p '<literal prompt>'`. Native Grok owns planning,
implementation, verification, and goal iteration; Orb creates no sentinel
loop or goal automation. It does not use OpenCode or the core model proxy.
An objective can require a validated GB10 implementation and reproducible
benchmarks; no elapsed wall-clock deadline is added by this adapter. Node
job time limits still apply, and interrupted goals can be resumed.

Install the native ARM64 CLI on DGX and configure the **node service** with
`SANDBOXED_NODE_GROK_HOME=/var/lib/sandboxed-node/.grok`. Provision a separate
cached login there as the service user using the official Grok login flow.
Keep `auth.json` owned by that user with mode `0600`; the directory must be
writable for token refresh and native session state. With systemd
`ProtectSystem=strict`, allow writes to `/var/lib/sandboxed-node`. Do not
copy refresh credentials shared by another machine. The job requests only
`managed_auth: ["grok"]`; the node injects the trusted `GROK_HOME` after
payload environment variables. Credential contents and the configured path
are absent from the job payload. Job cwd and HOME remain the mission directory.
This is service-account credential access for trusted remote jobs, not a
filesystem isolation boundary against code executing as that same account.

Deploy both core and node changes: heartbeat advertises `managed_auth`, and
`GET /jobs/:id/log?offset=N` returns bounded incremental log chunks. Missing
auth rejects submission; legacy nodes fail the CLI guard with exit 78.
Interactive login output triggers cancellation. A running CLI with no
model/tool progress for 120 seconds is cancelled with a startup diagnostic
(the timer excludes queued time). No automatic device-login flow is started.

Orb streams thought/text/tool events, suppresses the CLI's repeated final
text snapshot, and coalesces text/thought deltas into snapshots at chunk and
tool boundaries. Before submitting a new job, Core persists a preallocated
native session UUID under the current run generation and passes it with
`--session-id`. An interruption before the final stream event therefore
retains the native identity. `POST /api/control/missions/:id/resume` continues on the recorded node
using `--resume <session> -p '/goal resume'` for a goal, or passes explicit
request `content` verbatim. New sessions use only `--session-id`; continuation
uses only `--resume`. `/api/control/message` and targeted actor messages reject
remote follow-ups with a conflict directing callers to the node resume route;
they never start a local harness. PR/track writers and requests changing workspace or
writer identity require a linked replacement through normal create admission. It uses the same lease-before-submit fence as
initial dispatch. If the node or native session is missing, the API returns
`REMOTE_RESUME_REQUIRES_REPLACEMENT`: create a remote mission with
`supersedes_mission_id` pointing to the original. Do not silently resume
legacy remote work locally. A CLI exit alone is insufficient for goal
success: an `end` event with `end_turn` is also required.
