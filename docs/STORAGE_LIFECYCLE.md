# Storage lifecycle policy

Sandboxed.sh treats cleanup as an approval workflow, not an automatic disk
reclamation mechanism. The inventory is dry-run-only: it never deletes
workspaces, mission directories, Lean/build caches, worktrees, images, or
credentials. `--apply` is intentionally rejected.

Run it against explicit roots and retain the JSONL output as the operator audit
log:

```bash
python3 scripts/storage_inventory.py \
  --missions-dir /root/.sandboxed-sh/missions \
  --root /root/.sandboxed-sh/containers/example/workspaces \
  --root /root/.cache \
  --root /var/lib/machines \
  --retention-days 30 \
  --audit-log /var/log/sandboxed-sh/storage-inventory.jsonl
```

Each record identifies path, kind, size, age, persisted mission owner (the
mission-store key when known), mission/workspace IDs and status, and a policy
decision. `active`, `pending`, and `waiting_background` missions are always
`keep`; an unreadable store remains unattributed and must not be treated as an
approval candidate. Only an attributed, terminal `mission-*` directory past
retention is emitted as `would_remove`; caches, worktrees, images, and every
unattributed path remain `keep` until ownership is established. A
`would_remove` record is a candidate list for the listed owner to review
manually, not authority to delete.

The inventory also finds nested Lean (`.lake`, `.lean`) and common build-cache
directories under each supplied root. They receive their enclosing mission's
owner when one exists, but are still retained as separate review records so
their bytes can be attributed without authorizing cache deletion.

The background mission-directory retention sweep is also dry-run by default.
With cleanup enabled in Settings, it emits structured `mission GC audit` logs
with `action=would_remove`, ownership identifiers, size, path, and reason. It
will execute only if an operator explicitly sets `WORKSPACE_GC_EXECUTE=1` on
the service and restarts it. The default retention is one day for terminal
missions and seven days for AwaitingUser/Paused missions. The normal sweep is
hourly unless `WORKSPACE_GC_INTERVAL_MINUTES` is set. While disk pressure is at
Warn or Critical, the disk watcher requests the same configured sweep every
five minutes; a shared lock prevents overlapping scans.

Resource limits are separate from retention. Use `GET
/api/workspaces/:id/resources` to inspect effective memory/swap/CPU limits and
matching live scopes before changing a workspace shared by concurrent missions.

## Admission and continuity

By default, disk warning is not a portfolio-wide stop. Local mission placement
uses `MISSION_WORKSPACE_ROOT` when it names an existing writable absolute
directory (for example `/srv/sandboxed-storage`). The service canonicalizes the
directory, including a deliberate symlink, and falls back safely to the
workspace's recorded root if it is missing, unwritable, relative, or contains
path traversal. It never moves or deletes existing workspace records or active
mission directories; an already-existing legacy mission directory continues to
be used.

Admission and fleet health measure the `statvfs` filesystem that backs this
selected path, not `/`. Their non-secret output includes the canonical path,
filesystem identifier, free GiB, and required GiB. Set
`DISK_ADMISSION_AT_WARN=1` on build-heavy hosts to refuse all new missions at
Warn instead of waiting for Critical. A local disk-heavy mission should set
`estimated_disk_gib`; the API rejects it when its estimate would cross
`MISSION_DISK_EMERGENCY_RESERVE_GB` (default 150 GiB).
`MISSION_DISK_DEFAULT_ESTIMATE_GIB` defaults to 64 GiB, so a local request
without an explicit estimate remains fail-closed. Critical level always rejects
all new local missions.

For a high-churn Lean/build host, a deliberately aggressive profile is:

```bash
DISK_WARN_PCT=80
DISK_CRITICAL_PCT=88
DISK_ADMISSION_AT_WARN=1
MISSION_DISK_EMERGENCY_RESERVE_GB=200
MISSION_DISK_DEFAULT_ESTIMATE_GIB=64
WORKSPACE_GC_EXECUTE=1
WORKSPACE_GC_INTERVAL_MINUTES=10
```

Host-level caches and harness logs are outside the mission ownership model.
Install `scripts/storage_hard_cleanup.sh --apply` from an hourly systemd timer
on dedicated build hosts. It removes expired mission-store backups, old
OpenCode logs, reconstructible OpenCode cache entries, Hermes staging and
quarantine content, truncates oversized harness logs, and caps the journal.
Its retention and size controls are environment-configurable; run it without
`--apply` for an inventory.

Lean builds should use `remote-lean-build`. The wrapper can ship modified and
untracked regular source files as a bounded, content-hashed overlay on top of a
pinned commit; deletion, rename, symlink, `.git`, `.lake`, traversal and
oversized overlays fail closed. Remote placement reserves the declared scratch
estimate, while runners reuse content-addressed checkouts and dependency-only
Lake cache slots. This lets a dirty proof lane continue on a roomy node without
duplicating a fresh Mathlib tree on the production host.

## Future placement scoring (design only)

After the P0 filesystem admission work, placement can consume one unified
scorecard: hard constraints first (workspace isolation, reachable runner,
filesystem capacity after estimate plus reserve), then soft penalties for a
GitHub runner being busy or highly loaded. Busy/load must only rank otherwise
admissible candidates lower; it must never become an admission rejection or
weaken the local filesystem fail-closed checks.
