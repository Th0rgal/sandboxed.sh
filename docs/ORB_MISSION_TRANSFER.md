# Continue a mission on another machine

Status: deployed to production Core and all six nodes, 23 September 2026.
The footer now exposes **Change machine…**. This moves the existing mission,
preserving its ID. Export/import remains a separate record-copy operation.

## Operator flow

Click the machine label beside the harness in the mission footer. A menu lists
This computer, Core, and online nodes, with the current machine checked. Each
entry shows readiness for the selected harness and model. Offline, cordoned,
unauthenticated and unsupported targets explain why they cannot be selected.

Selecting a destination opens “Continue on <machine>” with:

- Conversation and workspace files selected by default.
- A file inventory, total bytes, excluded credentials/caches, and warnings for
  external paths or missing attachments. Nothing is silently truncated.
- Same harness/model by default; require an explicit replacement if unavailable.
- “Move mission” as the operation. Independent copies are outside this scope.

A running turn must finish or be stopped before the snapshot. Copying live files
while the agent writes is not a consistent checkpoint. Show preparation,
copy progress, destination validation, and readiness as separate stages. A
failed preparation leaves the source usable. Do not relabel the source machine
before a destination receipt exists.

## Semantics and architecture

Keep the mission ID, project, track, conversation history and links stable.
Transfer is a recorded action within the existing attempt, with a new execution
generation and placement after activation. Do not create a child mission or use
parent/supersedes relationships to simulate a move. Orb shows a machine-change
marker in the same conversation. Historical jobs and receipts retain their
original machine and generation; only the current execution placement changes.

The default is a fresh native harness session with the complete portable
conversation plus workspace snapshot. Resume native session data only when a
versioned compatibility check accepts the harness, session format, OS and path
mapping. Never promise live-process migration or silently switch models.

Preparation and activation must be separate. Existing `create_mission` with a
prompt dispatches immediately, so it cannot be used to launch first and copy
files afterwards. A transfer action owns an idempotency key, mission ID, source
revision and generation, destination, staged manifest and activation receipt
in the execution record. Commit placement with a compare-and-swap against the
source generation, and reject late job events or follow-ups for the old
generation. Every execution path, including local Orb, must honor that fence.
Reconcile interrupted actions from those receipts; do not keep the authoritative
state in browser localStorage.

Fence the source while preparing and validate the destination before committing
the new placement. Only one generation may execute. A retry reconciles the
activation receipt before starting anything; an ambiguous activation must not
restart the source. On failure before activation, release the fence and retain
the source placement. After activation, rollback requires confirmed destination
shutdown and a new generation. Retain the original workspace checkpoint for
recovery; do not delete mission history. Returning to a previously used machine
must stage a new generation instead of overwriting its old checkpoint in place.

## Data path

Use bounded, chunked, authenticated file transfer through Core and the selected
node; local Orb uses Tauri filesystem commands. Preserve relative paths, bytes,
executable bits and a SHA-256 manifest. Resolve attachments into the new root
and include an old-to-new path map in the portable context. Absolute external
paths require an explicit inventory decision, not a global text replacement.

Exclude runtime credentials and generated harness configuration; regenerate
configuration and use destination-owned authentication. Preserve source-control
history through an explicit Git bundle, excluding Git credential configuration
and hooks. Ignore caches/build outputs by default with a visible inventory.
Reject path traversal, unsafe symlinks, special files, changed-during-read files,
manifest mismatches, disk exhaustion and oversized snapshots before activation.
Chunk retries must be idempotent and support resuming interrupted uploads.

Existing `file-resources` and node `/jobs/:id/files` provide confined reads.
Existing `SourceArchive` is a pinned Git source archive for build jobs, not a
complete arbitrary mission-workspace archive. Existing `/api/uploads` provides
single-file staging, not atomic directory restore. Extend those primitives with
a checked manifest and staged restore rather than shell-generated `tar` commands
in mission prompts.

## Acceptance checks

Core → node, node → node, node → local, and local → Core must preserve binary
attachments, Unicode filenames, executable files, uncommitted edits and the
conversation. Verify that destination credentials are used and no source token
moves. Cover interrupted copy, duplicate activation, unavailable destination,
source edits during preparation, insufficient disk, and failure after destination
preparation but before placement is committed. Verify unchanged mission ID,
history and links, rejection of stale-generation events, transfer back to a
previous machine, and recovery after an ambiguous activation. A successful
receipt includes the mission ID, source/destination machines and generations,
manifest digest, byte/file totals, exclusions, context revision and actual
native-resume versus fresh-session mode.


## Implementation and current bounds

The HTTP contract is `GET/POST /api/control/missions/:id/machine-transfer`.
GET reports version 1, destinations and durable actions. POST accepts prepare,
source/destination file operations, local snapshot/verification receipts,
activate and cancel. Reopening the menu recovers a pending action. A failed
copy keeps the source fenced until retry or **Cancel**; cancellation restores
its availability without changing placement. Activation is idempotent.

SQLite's `machine_transfers` action journal and mission placement commit in one
transaction. An activation reserves a terminal generation in `mission_runs`.
Run acquisition rejects pending transfers and obsolete machine owners. Local
runs use `/client-run` permits, verified by Tauri before starting a CLI; normal
completion carries the same run ID/generation. Only a confirmed missing legacy
backend capability permits the old local launch path.

Orb first prepares an inventory (**Stop and prepare** for active turns), then
asks for **Move to <machine>**. No model call starts at activation. The next
message creates a fresh native session with portable event history; native CLI
session databases are never copied. The footer and file browser use the
committed destination, including before the first job runs there.

The shared Rust checkpoint adapter uses 1 MiB blocks, SHA-256 manifests,
resumable staged offsets and sealed verification receipts. Limits are 10 GiB,
50,000 files and an 8 MiB manifest. Portable event context is limited to
128 KiB / 50,000 events; exceeding a limit produces an explicit refusal,
never truncation. The source is rechecked before activation.

Git repositories, including nested repositories, travel as bundles; the
working tree is overlaid without checkout, preserving uncommitted contents.
Git credentials, hooks and runtime configuration are excluded. The index is
rebuilt from HEAD, so staged changes become unstaged. Empty directories and
repositories without any refs are not recreated as Git repositories.

Symlinks and special files currently require materialization before transfer.
Explicit uploaded-image references missing from the inventory, or outside the
known workspace roots, block preparation. Arbitrary external filesystem paths
are not followed or rewritten. This version does not import external attachments
on the user's behalf. CLI detection is used for readiness; Grok nodes also need
managed auth. Provider/model account validation remains with normal harness
admission. Node continuations currently support Codex, Grok and OpenCode.

Only machines attached to the same Core participate. Local transfers require
the participating Orb computer; closing that app pauses local file transport.
Uncertain native termination keeps the execution lease and prevents movement.
Source checkpoints are retained; this feature does not garbage-collect them.

Deploy the Core and node endpoints before the new Orb native binary/renderer.
A missing transfer capability disables the operation with an update message.
The automated checks cover SQLite restart/CAS/generation fences, binary and Git
round trips, integrity refusals, the authenticated node adapter/file browser,
client compatibility, component failure states and the footer flow in Playwright.
A live cross-host smoke test is still required when deploying these binaries.

## Deployment validation — 23 September 2026

Production originally returned HTTP 404 for the transfer route while Orb already
shipped its UI. The release was prepared in
`/srv/sandboxed-storage/build-source/machine-transfer` on agent-core, from the
running production source `cd15fc2ae`, preserving the newer fork occupancy fixes.
The isolated backend patch SHA-256 is
`ce27782e03987e87b02eadcff71509a953c68e6d6eef1e21b4311beb67f4490c`.

All six nodes (Ashur, Babylon, Nippur, old-agent, Sepolia and DGX Spark) were
updated after cordoning and checking zero active jobs/leases. Their authenticated
`/machine-transfer/capabilities` endpoints return version 1. They were uncordoned
only after health checks; their previous binaries are retained as
`/usr/local/bin/sandboxed-node.pre-machine-transfer`.

An isolated Core on loopback port 3005 verified actual transfers through
Core → old-agent → Core and Core → old-agent → DGX ARM → Ashur → Core.
The mission ID stayed stable, 256 binary byte values and a Unicode filename
survived, `.env` was excluded, and each activation remained `awaiting_user`.
No model was started. The seven Linux transfer tests passed. The macOS debug
Orb app bundle was rebuilt successfully.

Core activation initially returned HTTP 409 because conversation
`e19e93c0-6942-4f16-ba04-9adfcafed915` was executing. Thomas explicitly approved
forcing deployment. The guarded endpoint then installed the backend and companion
binaries; `hermes-assistant` was restarted. Both services are active and public
`/api/health` returns OK. The authenticated production transfer route returns
version 1 and all six node destinations available. The interrupted conversation
automatically resumed at generation 8 with healthy running execution.

The production binary SHA-256 is
`a4b197494f686931b4a952dfb7c4c42b23a07c97dc6841437d754628f007930a`.
The deploy event names base commit `cd15fc2aeabd`; the transfer changes are the
isolated patch identified above, applied on top of that base. The rebuilt Orb
bundle is available locally; an already-open transfer error can be retried
against the now-updated backend without relaunching the app.
