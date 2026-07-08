# Remote Mission Nodes

Sandboxed.sh still runs missions locally by default. The remote-node work in
this branch is a protocol skeleton for moving heavy missions to another machine
without changing the normal local/container execution path.

## Target Shape

- **Core** stays responsible for auth, mission records, workspace metadata,
  Library sync, event persistence, and the UI.
- **Remote node** is a small Rust service installed on a compute host. It exposes
  a narrow API to accept leases from core, start/stop mission processes, stream
  harness events back, and report capacity.
- **Leases** are short-lived capability tokens scoped to one mission. A node
  never receives broad dashboard credentials.
- **Heartbeats** report online/degraded/offline status, labels, CPU/RAM/disk/GPU
  capacity, and current mission counts.

## Rollout Phases

1. Report configured remote-node state in Hermes mission control. Scheduling is
   intentionally local-only.
2. Add a `sandboxed-node` daemon with `/heartbeat`, `/leases`, `/events`, and
   `/artifacts` endpoints.
3. Let core choose a node for eligible missions using workspace requirements,
   labels, and capacity.
4. Add UI controls for node health, drain mode, resource usage, and per-mission
   placement.

## Current Environment Flags

- `SANDBOXED_REMOTE_NODES_ENABLED=1` enables the reporting path.
- `SANDBOXED_REMOTE_NODES=http://node-a:9100,http://node-b:9100` records the
  configured endpoint count.

These flags do not change scheduling yet.
