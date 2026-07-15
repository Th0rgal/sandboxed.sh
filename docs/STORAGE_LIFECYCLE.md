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
the service and restarts it. Do not set that flag until the candidate report has
been reviewed and backed up; it is deliberately off by default.

Resource limits are separate from retention. Use `GET
/api/workspaces/:id/resources` to inspect effective memory/swap/CPU limits and
matching live scopes before changing a workspace shared by concurrent missions.
