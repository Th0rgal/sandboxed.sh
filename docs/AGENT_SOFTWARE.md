# Agent software in Machines

Machines includes a collapsed, connection-scoped **Agent software** accordion.
It reports local, Core and registered-node **host** installations. Container
images may contain other versions: an unreported launch version is shown as
unknown, never inferred from the host installation. SSH-only machines remain
visible with an explicit unavailable state.

## Inventory and compatibility

`GET /api/software?force=true` checks Core. Add `node=<registered ID>` to proxy
to that node's authenticated `/software` endpoint; node credentials never reach
the desktop. Orb uses the `software_inventory` native command, respecting its
configured executable overrides. Older binaries return “Runtime update required”.

The native monitor checks on connection and every 15 minutes, with a refresh
button bypassing caches. Active update jobs refresh every 10 seconds. Release
metadata comes from the official npm registry and is cached for six hours.
Unknown versions, offline machines and unavailable metadata are distinct states.
Release availability does not assert compatibility: activation has separate checks.

The runtime reports its compiled build identity, captured at process startup,
and detects a replaced executable that needs a restart. Launch records preserve
runner identity and the launch-time harness version when the execution boundary
knows the executable (Orb's native launches). Generic shell/node jobs and
container execution explicitly do not assert a host-derived harness version.
Local launch records are retained under `launches/`; `active/` receipts are
removed on execution completion. The default store is
`~/.local/state/agent-software`; services may set `AGENT_SOFTWARE_STATE_DIR`.
Runtimes sharing a set of mutable host executables must share that directory
and its permissions. Different service users should configure this explicitly.

## Manual updates

`POST /api/software/updates` accepts `{component, version, path}`;
`POST /api/software/updates/cancel` accepts `{id}`. The optional `node` query
parameter routes both operations. Equivalent native commands are
`software_update` and `software_cancel`.

The compact rows show normalized versions and the detected installer. Machines
without inventory are grouped into an expandable section.

Bun and pnpm installations use their own global package manager after checking
its configured bin directory. npm installations use the original verified global
prefix, including installations under a Homebrew prefix that are actually npm
packages. Native Claude installations use `claude install <version>`. Package
identity is checked against its manifest; an unknown layout is not guessed.
Bun detection examines the launcher as well as its resolved target, since a
custom Bun global directory can live outside `~/.bun`.

Fleet launchers under `/usr/local/bin` resolving into the matching
`/opt/sandboxed-tools/<harness>/` tree use the root-owned
`orb-update-harness` entry point. It accepts only an allowlisted harness and an
exact stable version, clears caller environment, and invokes the existing fleet
installer. Services need a narrow sudo grant for that entry point and write
access through their systemd filesystem namespace. This does not grant a shell
or arbitrary package installation. The installer verifies official artifacts and
CLI contracts, switches symlinks atomically, and retains live/previous versions.
All service users sharing those launchers use `/var/lib/orb-software` with a
shared group and default ACL, including Core and its colocated node.

These adapters retain the installation's ownership and verify the requested
version afterwards. Interrupted native-manager updates report failure and require
inspection/retry; they do not claim an atomic rollback. Genuine Homebrew packages,
unknown executables and service runtimes remain externally managed. The original
isolated Codex adapter remains available for compatible legacy npm/Orb launchers.

A durable job pins the exact version and previous symlink target. Duplicate
pending submissions reuse the job; cancellation is possible before installation.
A host-owned worker survives UI navigation. An execution/maintenance file lock
serializes activation against supported Orb, Core and node execution lanes;
a process check also defers maintenance for recognizable older/untracked agents.
A new launch during installation receives a maintenance error rather than
starting against an unverified installation.

The npm adapter stages into a unique directory, disables lifecycle scripts,
pins the official registry including the OpenAI scope, checks the exact installed
version and app-server capability, then atomically replaces the launcher symlink.
It rechecks the launcher before activation to detect external changes. Prior
package files remain intact. A failed activation restores the previous link.
An interrupted updater restores a staged launcher after obtaining the maintenance
lock and is marked failed rather than automatically replayed. New executions
remain fenced until that recovery completes.
The next explicit retry re-evaluates the current installation. Pending jobs wait
until a compatible native runtime is running again after the app is closed.

## Rollout

1. Build/relaunch Orb when local agents finish. Refreshing the web UI alone
   cannot load native commands or the maintenance lock.
2. Build and deploy Core and nodes with the existing guarded deployment process.
   Do not copy a macOS build to Linux or restart active workers.
3. Check inventory and executable ownership before using Update. A missing
   endpoint is a rollout status, not an empty inventory or an up-to-date result.

No installed harnesses are updated merely by opening Machines. Tests use isolated
fake npm packages and launchers; they do not modify global tools.
