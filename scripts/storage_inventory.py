#!/usr/bin/env python3
"""Read-only, dry-run-first storage attribution for sandboxed.sh.

This tool never removes data.  It inventories supplied roots, attributes
mission directories to persisted mission stores where possible, protects active
missions, and emits JSON Lines suitable for an operator audit trail.
"""
import argparse
import datetime as dt
import json
import os
from pathlib import Path
import sqlite3
import sys

ACTIVE = {"active", "pending", "waiting_background"}
TERMINAL = {"completed", "acknowledged", "failed", "interrupted", "blocked", "not_feasible"}


def iso_time(value):
    if not value:
        return None
    try:
        parsed = dt.datetime.fromisoformat(value.replace("Z", "+00:00"))
        return parsed.replace(tzinfo=dt.timezone.utc) if parsed.tzinfo is None else parsed
    except (AttributeError, ValueError):
        return None


def mission_index(missions_dir):
    """Return (prefix -> record, complete) from persisted stores, read-only.

    ``complete`` is false if any discovered store could not be read.  Callers
    must then retain every unowned directory: absence from a partial index is
    not evidence that a directory is stale.
    """
    records = {}
    complete = True
    missions_path = Path(missions_dir)
    if not missions_path.is_dir():
        print(json.dumps({"record_type": "audit", "action": "index_error", "path": str(missions_path), "error": "missions directory is missing or unreadable"}), file=sys.stderr)
        return records, False
    for db in missions_path.glob("missions-*.db"):
        try:
            conn = sqlite3.connect(f"file:{db}?mode=ro", uri=True)
            rows = conn.execute("SELECT id, status, updated_at, workspace_id FROM missions")
            for mission_id, status, updated_at, workspace_id in rows:
                prefix = mission_id[:8].lower()
                record = {"mission_id": mission_id, "status": status.lower(),
                          "updated_at": updated_at, "workspace_id": workspace_id,
                          "owner": db.stem.removeprefix("missions-"),
                          "short_id_collision": False}
                previous = records.get(prefix)
                if previous is not None:
                    record["short_id_collision"] = True
                    previous["short_id_collision"] = True
                if previous is None or (record["status"] in ACTIVE and previous["status"] not in ACTIVE):
                    records[prefix] = record
            conn.close()
        except (sqlite3.Error, OSError) as error:
            # Fail closed: an unreadable store does not create a cleanup
            # candidate; the corresponding directory remains unattributed.
            print(json.dumps({"record_type": "audit", "action": "index_error", "path": str(db), "error": str(error)}), file=sys.stderr)
            complete = False
    return records, complete


def bytes_under(path):
    total = 0
    for root, _, files in os.walk(path, followlinks=False):
        for filename in files:
            try:
                total += os.lstat(os.path.join(root, filename)).st_size
            except OSError:
                pass
    return total


def kind_for(path):
    name = path.name.lower()
    if name.startswith("mission-"):
        return "mission_dir"
    if name in {"target", ".cache", ".lean", ".lake", "cache"}:
        return "build_cache"
    if "image" in name or name in {"machines", "containers"}:
        return "container_image"
    if name in {"worktrees", ".git"} or (path / ".git").exists():
        return "git_worktree"
    return "unclassified"


def paths_to_inventory(root):
    """Yield direct children plus nested build-cache directories, once each.

    Direct children make roots such as workspace/container/image directories
    visible.  The targeted recursive walk captures `.lake`, `target`, and
    other nested build caches without producing a record for every source
    directory or following symlinks.
    """
    if root.name.startswith("mission-"):
        yield root
        return
    seen = set()
    for path in root.iterdir():
        if path.is_dir() and path not in seen:
            seen.add(path)
            yield path
    for current, dirs, _ in os.walk(root, followlinks=False):
        dirs[:] = [name for name in dirs if not os.path.islink(os.path.join(current, name))]
        for name in dirs:
            path = Path(current, name)
            if kind_for(path) == "build_cache" and path not in seen:
                seen.add(path)
                yield path


def mission_for_path(path, root, index):
    """Find a mission owner from this path or one of its ancestors."""
    current = path
    while True:
        if current.name.startswith("mission-"):
            return index.get(current.name.removeprefix("mission-").lower())
        if current == root:
            return None
        parent = current.parent
        if parent == current:
            return None
        current = parent


def inventory(root, index, index_complete, cutoff):
    root = Path(root)
    if not root.exists():
        yield {"record_type": "audit", "action": "root_missing", "path": str(root)}
        return
    for path in paths_to_inventory(root):
        kind = kind_for(path)
        mission = mission_for_path(path, root, index)
        mtime = dt.datetime.fromtimestamp(path.stat().st_mtime, tz=dt.timezone.utc)
        active = bool(mission and mission["status"] in ACTIVE)
        terminal = bool(mission and mission["status"] in TERMINAL)
        mission_updated_at = iso_time(mission["updated_at"]) if mission else None
        short_id_collision = bool(mission and mission["short_id_collision"])
        # Only a terminal, attributed mission directory is ever proposed.
        # Caches, worktrees, images, and unattributed directories remain
        # inventory-only even when old: an operator must establish ownership
        # before any separate cleanup action can be approved.
        eligible = bool(
            kind == "mission_dir"
            and index_complete
            and mission
            and not short_id_collision
            and terminal
            and mission_updated_at is not None
            and mission_updated_at < cutoff
        )
        if active:
            reason = "active_mission_protected"
        elif kind != "mission_dir":
            reason = "unattributed_requires_owner_approval"
        elif not index_complete:
            reason = "mission_attribution_incomplete"
        elif not mission:
            reason = "unattributed_requires_owner_approval"
        elif short_id_collision:
            reason = "short_id_collision_protected"
        elif not terminal:
            reason = "non_terminal_mission_protected"
        elif mission_updated_at is None:
            reason = "mission_timestamp_invalid"
        elif eligible:
            reason = "terminal_mission_past_retention"
        else:
            reason = "within_retention"
        yield {
            "record_type": "storage_item", "action": "would_remove" if eligible else "keep",
            "path": str(path), "kind": kind, "owner": mission["owner"] if mission else "unattributed",
            "mission_id": mission["mission_id"] if mission else None,
            "workspace_id": mission["workspace_id"] if mission else None,
            "mission_status": mission["status"] if mission else None,
            "age_days": round((dt.datetime.now(dt.timezone.utc) - mtime).total_seconds() / 86400, 2),
            "size_bytes": bytes_under(path), "reason": reason,
        }


def main():
    parser = argparse.ArgumentParser(description="Read-only sandboxed.sh storage inventory (never deletes).")
    parser.add_argument("--missions-dir", required=True, help="Directory containing missions-*.db")
    parser.add_argument("--root", action="append", required=True, help="Root to inventory; repeat for workspaces, caches, images, or worktrees")
    parser.add_argument("--retention-days", type=int, default=7)
    parser.add_argument("--audit-log", help="Append JSONL audit records to this file")
    parser.add_argument("--apply", action="store_true", help="Rejected: this tool is inventory-only")
    args = parser.parse_args()
    if args.apply:
        parser.error("--apply is intentionally unsupported; inventory never deletes data")
    now = dt.datetime.now(dt.timezone.utc)
    cutoff = now - dt.timedelta(days=args.retention_days)
    indexed, index_complete = mission_index(args.missions_dir)
    records = [{"record_type": "audit", "action": "inventory_started", "dry_run": True,
                "retention_days": args.retention_days, "at": now.isoformat()}]
    for root in args.root:
        records.extend(inventory(root, indexed, index_complete, cutoff))
    output = "".join(json.dumps(record, sort_keys=True) + "\n" for record in records)
    sys.stdout.write(output)
    if args.audit_log:
        with open(args.audit_log, "a", encoding="utf-8") as handle:
            handle.write(output)


if __name__ == "__main__":
    main()
