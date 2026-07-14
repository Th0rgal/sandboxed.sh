#!/usr/bin/env python3
import json
from pathlib import Path
import sqlite3
import subprocess
import sys
import tempfile
import time

SCRIPT = Path(__file__).with_name("storage_inventory.py")

with tempfile.TemporaryDirectory() as temp:
    root = Path(temp)
    missions = root / "missions"; missions.mkdir()
    db = missions / "missions-thomas.db"
    conn = sqlite3.connect(db)
    conn.execute("CREATE TABLE missions (id TEXT, status TEXT, updated_at TEXT, workspace_id TEXT)")
    conn.execute("INSERT INTO missions VALUES (?, ?, ?, ?)", ("aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa", "active", "2026-01-01T00:00:00Z", "ws-a"))
    conn.execute("INSERT INTO missions VALUES (?, ?, ?, ?)", ("bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb", "completed", "2025-01-01T00:00:00Z", "ws-b"))
    conn.commit(); conn.close()
    workspaces = root / "workspaces"; workspaces.mkdir()
    active = workspaces / "mission-aaaaaaaa"; active.mkdir(); (active / "live").write_text("x")
    old = workspaces / "mission-bbbbbbbb"; old.mkdir(); (old / "old").write_text("x" * 32)
    cache = workspaces / "cache"; cache.mkdir(); (cache / "artifact").write_text("x" * 32)
    lean_cache = old / ".lake"; lean_cache.mkdir(); (lean_cache / "olean").write_text("x" * 32)
    old_time = time.time() - 20 * 86400; __import__("os").utime(old, (old_time, old_time))
    __import__("os").utime(cache, (old_time, old_time))
    # This reproduces the prior predicate bug: an old cache nested below a
    # terminal mission inherited its owner and was emitted as would_remove.
    __import__("os").utime(lean_cache, (old_time, old_time))
    result = subprocess.run([sys.executable, str(SCRIPT), "--missions-dir", str(missions), "--root", str(workspaces), "--retention-days", "7"], check=True, capture_output=True, text=True)
    rows = [json.loads(line) for line in result.stdout.splitlines()]
    items = {Path(row["path"]).name: row for row in rows if row["record_type"] == "storage_item"}
    assert items["mission-aaaaaaaa"]["action"] == "keep"
    assert items["mission-aaaaaaaa"]["owner"] == "thomas"
    assert items["mission-bbbbbbbb"]["action"] == "would_remove"
    assert items["cache"]["action"] == "keep"
    assert items["cache"]["reason"] == "unattributed_requires_owner_approval"
    assert items[".lake"]["kind"] == "build_cache"
    assert items[".lake"]["owner"] == "thomas"
    assert items[".lake"]["action"] == "keep"

    worktree = workspaces / "worktrees"; worktree.mkdir(); (worktree / ".git").write_text("gitdir: elsewhere")
    image = workspaces / "images"; image.mkdir()
    unknown = workspaces / "unattributed-old"; unknown.mkdir()
    for path in (worktree, image, unknown):
        __import__("os").utime(path, (old_time, old_time))
    result = subprocess.run([sys.executable, str(SCRIPT), "--missions-dir", str(missions), "--root", str(workspaces), "--retention-days", "7"], check=True, capture_output=True, text=True)
    rows = [json.loads(line) for line in result.stdout.splitlines()]
    items = {Path(row["path"]).name: row for row in rows if row["record_type"] == "storage_item"}
    for name in (".lake", "worktrees", "images", "unattributed-old"):
        assert items[name]["action"] == "keep", name

    broken = missions / "missions-broken.db"
    broken.write_text("not sqlite")
    result = subprocess.run([sys.executable, str(SCRIPT), "--missions-dir", str(missions), "--root", str(workspaces), "--retention-days", "7"], check=True, capture_output=True, text=True)
    rows = [json.loads(line) for line in result.stdout.splitlines()]
    items = {Path(row["path"]).name: row for row in rows if row["record_type"] == "storage_item"}
    assert items["mission-bbbbbbbb"]["action"] == "keep"
    assert items["mission-bbbbbbbb"]["reason"] == "mission_attribution_incomplete"
print("storage inventory regression test passed")
