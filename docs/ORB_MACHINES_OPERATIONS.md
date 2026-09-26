# Orb machines: storage and harness operations

## Incident, 22 September 2026

Core rejected a mission with 127 GiB free because admission reserves 64 GiB
of scratch plus a 64 GiB emergency floor. The check correctly measured `/root`;
production had not enabled the existing `MISSION_WORKSPACE_ROOT` override.
The root volume had about 129 GiB free during inspection, while the separate
data volume had about 1.5 TiB free.

Root usage was principally `/opt` (250 GiB), `/tmp` (185 GiB), `/root`
(96 GiB, including 46 GiB of named workspaces), `/var` (89 GiB, including
23 GiB of logs), and `/workspaces` (58 GiB). Mission database deletion would
not address these paths. Some deployment trees contained database backups,
not disposable compilation output.

Completed operations:

- Removed 14 GiB of inactive Rust incremental output from
  `/opt/sandboxed-sh-build/target/debug/incremental`, after checking process
  working directories, executables, and open descriptors.
- Moved the remaining shared build target to
  `/srv/sandboxed-storage/build-cache/orb-target`, preserving the old path
  with a symlink. The existing Orb build target also resolves there.
- Preserved the 43 GiB `native-continuity-prod-20260910-rollback` archive
  under `/srv/sandboxed-storage/archives`, retaining a compatibility symlink.
- Compressed the closed rotated syslog, then changed rsyslog rotation to
  daily/256 MiB, seven compressed rotations, checked hourly. Backed up the
  previous policy under `/root/backups/rsyslog.before-orb-storage`.
- Added the data volume to both mirrored disk-sentinel scripts.
- Installed native Codex 0.155.1 on core, Ashur, Babylon, Nippur, old-agent,
  and DGX Spark, verified as the `sandboxed-node` user, and enabled the
  daily `sandboxed-codex-update.timer` on each.

Root free space reached about 222 GiB. Existing source trees, mission
history, workspace registrations, credentials, and running missions were
preserved. Thirteen containers were still running during inspection; the
host-first direction is not evidence that those containers are unused.

The backend environment now specifies
`MISSION_WORKSPACE_ROOT=/srv/sandboxed-storage/host-missions` and
`TMPDIR=/srv/sandboxed-storage/host-tmp`. These changes are active after the guarded backend deployment on 22 September.
The live health endpoint now measures `/srv/sandboxed-storage/host-missions`
and reports about 1,451 GiB free. Existing generated mission paths retain
their persisted location. Explicit project working directories also retain
their configured location.

## Two launch defects

The backend advertised a global allowlist of Claude Code, OpenCode, and Grok.
Codex was absent regardless of what was installed on a node. This change
adds native `codex exec --json` launches, streamed thread/tool/text events,
resume on the original node, model/effort/fast-mode settings, and a scoped
core proxy key. Remote Codex `/goal` is explicitly refused until the node
supports Codex's app-server goal protocol; ordinary exec completion is not
proof that a native goal completed.

The Responses proxy additionally relied on CLIProxyAPI's expired Codex
login. When sandboxed.sh owns the account, native Responses now uses the
same account selection and locked refresh path as the local Codex driver.
Rotating ChatGPT credentials never go to leaf nodes. CLIProxy-owned accounts
continue through CLIProxyAPI. Subscription Responses supports streaming and
client-side input replay, not `previous_response_id` or server-side storage.

The `@controller` launch failure had a separate cause: the attachment
writer rejected the deliberate `/root/.sandboxed-sh` storage symlink with
`ENOTDIR`. It now resolves the operator-controlled storage root before its
descriptor-based path traversal. Symlinks beneath that boundary remain
rejected. Regression coverage includes the operator's French Pareto prompt,
controller snapshots, message attachments, and descendant symlink rejection.

## Architecture and ownership

Keep the existing portfolio → project → track → attempt → action → receipt →
evidence model. A machine is an execution resource; its workspace is an
attempt's directory/isolation policy. Neither is another project-state store.
Host-first means per-attempt host directories with resource limits and a
durable node/job identity. It does not mean a shared `/root` scratch area or
discarding resumable sessions and evidence.

Orb currently combines hardcoded SSH hosts, localStorage bookmarks, and the
server's remote-node roster. Editing a bookmark does not enroll a node or
install a harness. The green SSH bookmark dot is not a health check. Global
transport support also does not prove a particular node has a working CLI,
authentication route, disk budget, or compatible protocol.

The newer **This computer** path launches locally installed CLIs through Orb.
Keep that client-owned execution target separate from server-enrolled machines;
its readiness comes from local CLI probes and its attempts still belong to the
same project record. Fleet installation actions must never silently install or
change tools on the operator’s Mac.

Use the existing server node registry as the canonical fleet roster. Keep
local SSH bookmarks visibly separate as **Not enrolled**. Preserve node IDs
such as the existing `sepolia`; display `agent-core` as its operator label
rather than creating a duplicate machine.

## Proposed machines page

One row per registered machine:

| Field | Display |
| --- | --- |
| Identity | Display name, stable node ID, control-plane/compute role |
| Readiness | Ready, busy, draining, offline, or needs attention; observation age |
| Capacity | Running/available slots; CPU and memory |
| Storage | Execution filesystem, available space, emergency floor, reserved scratch |
| Harnesses | Codex/Claude/Grok/OpenCode chips with observed version and readiness |
| Updates | Current versus desired version, last check, pending update or failed canary |

Selecting a machine opens three sections:

1. **Execution:** working root, per-mission isolation and limits, active jobs,
   drain/undrain, latest successful launch and resume canary.
2. **Tools:** installed/desired versions, compatibility status, credential
   owner and auth readiness (never secrets), Install, Update, Pin, Roll back,
   and Test. A missing CLI, expired login, exhausted quota, and unsupported
   backend protocol must be distinct states.
3. **Storage:** one row per filesystem (deduplicate mounts), a breakdown of
   source/session data, artifacts, caches, logs, archives, and unknown data.
   Show a cleanup preview with bytes, ownership, retention, active leases,
   dirty/unpushed work, and the exact keep/delete reason.

The composer should show the selected machine's actual execution filesystem
and available budget before Send, and offer a ready alternative when the
selected harness cannot run there. Resource estimates should be explicit
for build-heavy work; lowering the emergency floor is not the default repair.

## Backend contract to add next

Extend existing node heartbeats with an observed `harnesses` map containing
version, executable path, protocol features (exec/resume/app-server/goals),
auth route/readiness, last probe time, and probe failure reason. Add filesystem
observations for execution and cache roots. Compute launchability from those
observations plus the core adapter capability and provider availability.

Implement tool installation/update and storage cleanup as typed core-owned
actions against the existing node registry. Requests carry idempotency keys
and desired versions or a reviewed cleanup-plan ID; responses return durable
action IDs. Receipts record before/after version or bytes, artifact digest,
verification, skipped active resources, and rollback outcome. Orb observes
those receipts; it does not run ad-hoc SSH installation commands or maintain
a second desired-state database in localStorage.

For production tool updates, stage an integrity-checked artifact, validate the
CLI contract, run a bounded canary, atomically switch the selected version,
and retain rollback plus versions used by live processes. Require drain only
for node-daemon/protocol changes or upgrades incompatible with existing
sessions. Batch updates should canary one node before rolling through the
remaining eligible fleet. The interim installer in this change checks the
official npm package's SHA-512 integrity and exec/resume CLI contract and
retains recent/in-use versions. A root-owned
`/etc/sandboxed-codex-update.env` can pin `CODEX_NODE_VERSION`.

For cleanup, keep mission metadata and evidence independently of scratch.
Never delete active leases, resumable session state, dirty/unpushed source,
or unknown ownership automatically. Apply TTL and high-water reclamation to
declared disposable caches; archive artifacts and verified clean terminal
attempt directories before retention cleanup. Retire old container workspace
registrations only after checking canonical path aliases, active processes,
live mission references, saved sessions, and unexported source/artifacts.

The existing storage inventory is read-only and the workspace GC defaults to
dry-run. Do not silently turn on whole-workspace deletion as a disk repair.

## Validation and deployment

Production runs the tested Linux debug backend from `8dd510f8a72d`, with
companion binaries from `fad8ed6a65bb`. The final follow-up preserves Codex
OAuth metadata during provider-chain resolution; without that flag the
Responses adapter could not select the core-owned account. Installed binary hashes
match the staged artifacts. The two existing agent sessions were paused for
maintenance and resumed with their native session IDs preserved. The older
process took longer than the initial health wait to drain; the guarded restart
completed and backend/Hermes health was checked afterward.

A scheduled production request using the exact French Pareto prompt,
`verity-pareto`, Codex, GPT-6 Astra, Medium, and a controller attachment was
accepted. Its payload was persisted on the data volume. The temporary mission
was deleted before execution, so this check did not start another audit.

Validation: 95 Codex tests, 14 attachment tests, 84 proxy tests, 135 remote
tests plus the corrected raw-Claude lifecycle fixture passed; two remote
tests remain intentionally ignored. The Orb launch/connection suite passed
37 tests, and the client production build passed.

The 24 provider-health tests passed after the resolver correction. A native
Codex request from Ashur through the public core proxy reached OpenAI and
returned the account’s usage-limit error (reset reported as 26 September),
rather than the previous unsupported-protocol/502 failure. This first probe
only reached Thomas’s exhausted account. The subsequent account-recovery
investigation below supersedes the initial quota diagnosis. No credits were
purchased.

The attachment canary was repeated successfully after the final deployment.
Core reports approximately 1,453 GiB free on the execution volume. The second
deployment waited for the active mission to finish naturally before the
guard admitted it.


### Codex account recovery and quota visibility

The provider store held an expired Ben credential while the account-specific
shared Codex home held a newer, valid generation for the same ChatGPT identity.
The launcher filtered the stale store entry before considering the shared
credential, leaving only Thomas’s exhausted account. A direct GPT-6 Astra
probe confirmed Ben had 45% of the weekly quota used (55% available); Thomas
reported 100%. These are measurements at repair time, not fixed allowances.

The backend now reconciles newer credentials under the existing per-account
and cross-process refresh locks before provider listing, usage probing,
native proxy routing and proactive refresh. Launcher account enumeration also
considers the same-account shared credential before excluding expired entries.
Recovery requires a matching ChatGPT identity, a newer unexpired JWT and a
nonempty refresh token; it never copies credentials across accounts.

Orb now waits for per-account Codex usage after the initial bulk cache response
and refreshes the display periodically. It shows Codex percentages in the
account rows, distinguishes exhausted quota from connected credentials, uses
the provider’s actual window duration, and omits zero-duration windows. Open
usage details remain reactive when updated measurements arrive.

Validation: four OAuth recovery/deadletter tests, 97 Codex tests and 278 Orb
tests passed. The merged client build and 13 affected UI tests also passed.

Production deployment `90707ecfc79a` passed the guarded idle check. The Pareto
Claude mission was paused and resumed with its original session preserved.
The production usage endpoints now return Ben at 45% used and Thomas at 100%,
both on 10,080-minute weekly windows. A native Codex GPT-6 Astra invocation on
Ashur through the core proxy completed successfully, then resumed the same
thread and recalled its marker; both processes exited zero. The temporary
proxy key was revoked. Core health reports `ok` with 1,452 GiB free on the
execution volume.

A second live canary exercised the exact Core harness path: Codex, GPT-6 Astra,
Medium. Its assistant response was `CORE_CODEX_OK`, with structured success and
`TurnComplete` evidence. The temporary mission was removed after verification.
Hermes also completed its graceful restart and both services are active.
