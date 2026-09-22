# Continue a mission on another machine

Status: implementation proposal, 22 September 2026. The machine label is not yet
an interactive transfer control. Existing mission export/import only copies the
record; it does **not** transfer workspace files or a running process.

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
- “Transfer” as the primary operation, “Create an independent copy” as secondary.

A running turn must finish or be stopped before the snapshot. Copying live files
while the agent writes is not a consistent checkpoint. Show preparation,
copy progress, destination validation, and readiness as separate stages. A
failed preparation leaves the source usable. Do not relabel the source machine
before a destination receipt exists.

## Semantics and architecture

Keep the project and track stable. A destination execution is a new linked
attempt, using the existing parent/supersedes relationships. Orb presents it as
one continued conversation with a visible machine-change marker and accessible
prior history. Do not change the meaning of an existing mission's execution
receipts or create a second writable project record.

The default is a fresh native harness session with the complete portable
conversation plus workspace snapshot. Resume native session data only when a
versioned compatibility check accepts the harness, session format, OS and path
mapping. Never promise live-process migration or silently switch models.

Preparation and activation must be separate. Existing `create_mission` with a
prompt dispatches immediately, so it cannot be used to launch first and copy
files afterwards. A transfer action owns an idempotency key, source revision,
destination, staged manifest and activation receipt in the execution record.
Reconcile interrupted actions from those receipts; do not keep the authoritative
state in browser localStorage.

For Transfer, fence the source while preparing, activate the destination only
after validation, then mark the source superseded. On failed activation retain
the source checkpoint and allow an explicit retry. Independent copy leaves the
source available, with a clear warning about concurrent edits to external repos.
Neither operation deletes the original workspace or history.

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
creation but before the source is superseded. A successful receipt includes the
source/destination attempt IDs, manifest digest, byte/file totals, exclusions,
context revision and actual native-resume versus fresh-session mode.
