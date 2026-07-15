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
    conn.execute("INSERT INTO missions VALUES (?, ?, ?, ?)", ("cccccccc-cccc-cccc-cccc-cccccccccccc", "completed", "2999-01-01T00:00:00Z", "ws-c"))
    conn.execute("INSERT INTO missions VALUES (?, ?, ?, ?)", ("dddddddd-dddd-dddd-dddd-dddddddddddd", "paused", "2025-01-01T00:00:00Z", "ws-d"))
    conn.execute("INSERT INTO missions VALUES (?, ?, ?, ?)", ("eeeeeeee-eeee-eeee-eeee-eeeeeeeeeeee", "awaiting_user", "2025-01-01T00:00:00Z", "ws-e"))
    conn.execute("INSERT INTO missions VALUES (?, ?, ?, ?)", ("abababab-abab-abab-abab-abababababab", "acknowledged", "2025-01-01T00:00:00Z", "ws-ack"))
    conn.execute("INSERT INTO missions VALUES (?, ?, ?, ?)", ("ffffffff-ffff-ffff-ffff-ffffffffffff", "failed", "not-a-timestamp", "ws-f"))
    conn.execute("INSERT INTO missions VALUES (?, ?, ?, ?)", ("99999999-0000-0000-0000-000000000001", "completed", "2025-01-01T00:00:00Z", "ws-g"))
    conn.execute("INSERT INTO missions VALUES (?, ?, ?, ?)", ("99999999-0000-0000-0000-000000000002", "paused", "2025-01-01T00:00:00Z", "ws-g"))
    conn.commit(); conn.close()
    workspaces = root / "workspaces"; workspaces.mkdir()
    active = workspaces / "mission-aaaaaaaa"; active.mkdir(); (active / "live").write_text("x")
    old = workspaces / "mission-bbbbbbbb"; old.mkdir(); (old / "old").write_text("x" * 32)
    recently_updated = workspaces / "mission-cccccccc"; recently_updated.mkdir()
    paused = workspaces / "mission-dddddddd"; paused.mkdir()
    awaiting_user = workspaces / "mission-eeeeeeee"; awaiting_user.mkdir()
    acknowledged = workspaces / "mission-abababab"; acknowledged.mkdir()
    invalid_timestamp = workspaces / "mission-ffffffff"; invalid_timestamp.mkdir()
    collided = workspaces / "mission-99999999"; collided.mkdir()
    cache = workspaces / "cache"; cache.mkdir(); (cache / "artifact").write_text("x" * 32)
    lean_cache = old / ".lake"; lean_cache.mkdir(); (lean_cache / "olean").write_text("x" * 32)
    old_time = time.time() - 20 * 86400; __import__("os").utime(old, (old_time, old_time))
    __import__("os").utime(recently_updated, (old_time, old_time))
    for path in (paused, awaiting_user, acknowledged, invalid_timestamp, collided):
        __import__("os").utime(path, (old_time, old_time))
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
    assert items["mission-cccccccc"]["action"] == "keep"
    assert items["mission-cccccccc"]["reason"] == "within_retention"
    assert items["mission-dddddddd"]["action"] == "keep"
    assert items["mission-dddddddd"]["reason"] == "non_terminal_mission_protected"
    assert items["mission-eeeeeeee"]["action"] == "keep"
    assert items["mission-eeeeeeee"]["reason"] == "non_terminal_mission_protected"
    assert items["mission-abababab"]["action"] == "keep"
    assert items["mission-abababab"]["reason"] == "non_terminal_mission_protected"
    assert items["mission-ffffffff"]["action"] == "keep"
    assert items["mission-ffffffff"]["reason"] == "mission_timestamp_invalid"
    assert items["mission-99999999"]["action"] == "keep"
    assert items["mission-99999999"]["reason"] == "short_id_collision_protected"
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

    legacy = missions / "missions-legacy.json"
    legacy.write_text('{"missions": {}}')
    audit_log = root / "storage-audit.jsonl"
    result = subprocess.run([sys.executable, str(SCRIPT), "--missions-dir", str(missions), "--root", str(workspaces), "--retention-days", "7", "--audit-log", str(audit_log)], check=True, capture_output=True, text=True)
    rows = [json.loads(line) for line in result.stdout.splitlines()]
    audit_rows = [json.loads(line) for line in audit_log.read_text().splitlines()]
    assert any(
        row.get("action") == "index_error" and row.get("path") == str(legacy)
        for row in rows
    )
    assert audit_rows == rows
    items = {Path(row["path"]).name: row for row in rows if row["record_type"] == "storage_item"}
    assert items["mission-bbbbbbbb"]["action"] == "keep"
    assert items["mission-bbbbbbbb"]["reason"] == "mission_attribution_incomplete"
    legacy.unlink()

    broken = missions / "missions-broken.db"
    broken.write_text("not sqlite")
    result = subprocess.run([sys.executable, str(SCRIPT), "--missions-dir", str(missions), "--root", str(workspaces), "--retention-days", "7"], check=True, capture_output=True, text=True)
    rows = [json.loads(line) for line in result.stdout.splitlines()]
    items = {Path(row["path"]).name: row for row in rows if row["record_type"] == "storage_item"}
    assert items["mission-bbbbbbbb"]["action"] == "keep"
    assert items["mission-bbbbbbbb"]["reason"] == "mission_attribution_incomplete"
print("storage inventory regression test passed")
