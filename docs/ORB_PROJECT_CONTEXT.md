# Shared project context and offline local starts

Project context is a writable shared directory per project and host. It is not
copied into each mission. `@context`, `@context/notes`, and quoted paths such as
`@"context/research notes.md"` resolve to that host's real directory. Ordinary
file attachments retain their existing snapshot behavior.

## Storage and synchronization

Core keeps files at `.sandboxed-sh/project-files/<project>` and the version
journal/blobs in the sibling `project-context-state/<project>` directory.
Nodes keep an endpoint-scoped replica below their work root. Orb keeps replicas
in `~/.orb/project-context/<server>/<account>/<project>`. Metadata, credentials,
and blobs are outside the agent-visible files directory.

Core observes direct edits every two seconds. Nodes and Orb replicas reconcile
on the same interval, recording pending operations before sending them. Each
operation has an ID and expected file revision. Lost responses are replayed
idempotently. Immutable SHA-256 blobs are verified on download. A replica is
usable offline only after a complete initial synchronization.

Concurrent changes preserve the first accepted shared version and the other
variant. Orb's file history offers comparison, restore, keep shared, use variant,
and keep both. Local editor races preserve a separately named conflict file.
Changes during download are not overwritten. A missing/replaced context root
stops synchronization instead of broadcasting deletion of the whole tree.

Limits: 10 MiB per file, 100 MiB current project contents, 5,000 entries.
Hidden files, common build directories, credentials, symlinks and special files
are excluded or refused. History retains at least 90 days and 20 changes per
path; immutable blobs are conservatively retained rather than garbage-collected
while disconnected replicas may still reference them. Text preview is bounded
at 512 KiB. Polling observes stable filesystem states, not every transient write.

Linux container workspaces mount only their project's live directory at
`/run/sandboxed-context/<project>`. Mounting pins filesystem and namespace file
descriptors and uses `open_tree`/`move_mount`; unsupported kernels fail explicitly.
Node replication uses a project-scoped grant that cannot access mission APIs,
other projects, history resolution, or arbitrary files. Grants expire after 30
days and are refreshed when the project is prepared for another node mission.
Removing the node from Core revokes its grant.

## Offline local missions

A new local mission gets its UUID, initial run UUID, machine identity, private
execution directory and disk journal from the native runner before the CLI is
started. A draft idempotency key prevents retrying a lost native response from
starting another CLI. Existing mission IDs and native session IDs cannot be
passed through this creation authority.

Execution directories are `~/.orb/local-runs/<mission UUID>`. Context paths remain
shared across those private directories. The native journal is account/endpoint
scoped at `~/.orb/local-origins/`. It captures the initial prompt, cumulative
assistant output, terminal state and ordered snapshot numbers. Native recording
continues through webview reloads; pending records are reopened when Orb starts.
Logout pauses uploads, not a running CLI or its local recording.

`POST /api/control/local-origins` atomically imports the mission, execution fence,
transcript and snapshot number. It never starts a harness. UUID collisions,
changed origin metadata and stale execution owners are refused. Replaying a
completed snapshot is harmless; a later snapshot cannot reactivate it. Cached
project/model catalogs and the local journal keep the UI usable when Core is
unreachable. The CLI may still require connectivity to its model provider.

**Resuming existing missions still requires Core.** A crashed native process is
not automatically relaunched. Existing reconciliation checks for a surviving
process before releasing an execution fence. Pending local output is retained;
a full operating-system crash can lose output since the last journal snapshot.

## Rollout and verification

Requires matching Core, node and Orb native builds. Older native builds retain the existing online launch path; offline attempts
surface an update message and preserve the draft. This source change does not
restart an already-running Orb instance or production missions.

Validation covers store conflicts and path safety, HTTP replica convergence,
offline queue recovery, file/folder transitions, grant scope, atomic offline
import/replay/collisions, local UI fallback/catalog caching, and the browser
comparison panel. Linux mount creation and repeat mounting were exercised in an
isolated Docker container; deployment-specific nspawn namespaces and live fleet
convergence still require a coordinated rollout check.
