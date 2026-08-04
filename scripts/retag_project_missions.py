#!/usr/bin/env python3
"""Collapse `<family>-<suffix>` mission projects into `<family>` + a track prefix.

Why this exists
---------------
Controllers ask for their project by family name (`verity`), but missions were
tagged with whatever the mission author typed: `verity-core`, `verity-phase1d`,
`verity-benchmark`. An exact `project` filter therefore returns a fraction of a
project's work, and the fraction it returns is not predictable. `project_prefix`
papers over this at read time; this script fixes it at the source, so
`project=verity` alone is sufficient.

The transformation, per mission::

    project=verity-phase1d  track=core-c3   ->  project=verity  track=phase1d/core-c3
    project=verity-core     track=None      ->  project=verity  track=core
    project=Verity          track=x         ->  project=verity  track=x

The suffix is preserved rather than dropped, so no two source slugs can collide
in the result, and the operation is reversible from the journal it writes.

Safety
------
* Dry run is the default. Writing requires ``--apply``.
* Nothing is guessed. Families must be named with ``--family``; run without any
  to get an inventory and a list of *candidates* to arbitrate by hand.
* Every applied change is appended to a JSONL journal **before** the next one is
  attempted, and ``--undo`` replays it backwards. A crashed run is recoverable
  from the journal alone, without a database restore.
* A family that is a prefix of another family name is rejected rather than
  applied, because the hyphen-anchored match would silently swallow it.
"""

from __future__ import annotations

import argparse
import json
import os
import sys
import time
import urllib.error
import urllib.parse
import urllib.request
from collections import Counter
from concurrent.futures import ThreadPoolExecutor
from typing import Any, Dict, Iterable, List, Optional, Tuple

PAGE = 200
# One pass can miss a mission that a running controller re-touched mid-scan:
# the listing is ordered by `updated_at`, so a concurrent update can migrate a
# row into a page that was already consumed. Re-scanning until a pass is clean
# converges instead of pretending a single pass was authoritative.
MAX_PASSES = 6


class Api:
    def __init__(self, base: str, token: str, timeout: int = 60) -> None:
        self.base = base.rstrip("/")
        self.token = token
        self.timeout = timeout

    def _request(self, method: str, path: str, body: Optional[dict] = None) -> Any:
        data = json.dumps(body).encode() if body is not None else None
        req = urllib.request.Request(self.base + path, data=data, method=method)
        req.add_header("Authorization", "Bearer " + self.token)
        if data is not None:
            req.add_header("Content-Type", "application/json")
        with urllib.request.urlopen(req, timeout=self.timeout) as resp:
            raw = resp.read().decode()
        return json.loads(raw) if raw else None

    def missions(self, params: str) -> List[dict]:
        out = self._request("GET", "/api/control/missions?" + params)
        if isinstance(out, dict):
            out = out.get("missions") or []
        return out or []

    def set_project(self, mission_id: str, project: Optional[str],
                    track: Optional[str]) -> None:
        self._request(
            "POST", "/api/control/missions/%s/project" % mission_id,
            {"project": project, "track": track},
        )


def fetch_family(api: Api, family: str) -> List[dict]:
    """Every mission in `family` or `family-*`, paged to exhaustion."""
    found: Dict[str, dict] = {}
    offset = 0
    while True:
        page = api.missions(
            "project_prefix=%s&limit=%d&offset=%d"
            % (urllib.parse.quote(family), PAGE, offset)
        )
        if not page:
            break
        for mission in page:
            found[mission["id"]] = mission
        if len(page) < PAGE:
            break
        offset += PAGE
    return list(found.values())


def plan_change(mission: dict, family: str) -> Optional[Tuple[str, str, Optional[str]]]:
    """(mission_id, new_project, new_track), or None when already correct."""
    project = (mission.get("project") or "").strip()
    track = (mission.get("track") or "").strip() or None
    if not project:
        return None

    lowered = project.lower()
    fam = family.lower()
    if lowered == fam:
        # Already collapsed; only a case variant is left to normalise.
        if project == family:
            return None
        return (mission["id"], family, track)
    if not lowered.startswith(fam + "-"):
        return None

    suffix = project[len(family) + 1:].strip("-")
    if not suffix:
        return None
    new_track = "%s/%s" % (suffix, track) if track else suffix
    return (mission["id"], family, new_track)


def inventory(api: Api) -> Counter:
    """Distinct project values with counts, for arbitration."""
    counts: Counter = Counter()
    offset = 0
    while True:
        page = api.missions("limit=%d&offset=%d" % (PAGE, offset))
        if not page:
            break
        for mission in page:
            counts[(mission.get("project") or "").strip() or "(none)"] += 1
        if len(page) < PAGE:
            break
        offset += PAGE
    return counts


def suggest_families(counts: Counter) -> List[Tuple[str, List[str]]]:
    groups: Dict[str, List[str]] = {}
    for slug in counts:
        if slug == "(none)":
            continue
        root = slug.split("-", 1)[0].lower()
        groups.setdefault(root, []).append(slug)
    return sorted(
        ((root, sorted(slugs)) for root, slugs in groups.items() if len(slugs) > 1),
        key=lambda item: -sum(counts[s] for s in item[1]),
    )


def reject_overlapping(families: List[str]) -> None:
    """`verity` and `verity-benchmark` cannot both be families.

    Collapsing `verity` would eat `verity-benchmark`'s missions on the way past,
    and which one wins would depend on the order the families were listed.
    """
    lowered = [f.lower() for f in families]
    for i, a in enumerate(lowered):
        for j, b in enumerate(lowered):
            if i != j and b.startswith(a + "-"):
                raise SystemExit(
                    "ABORT: family %r is a prefix of family %r — collapsing the "
                    "first would absorb the second. Pick one." % (families[i], families[j])
                )


def apply_changes(api: Api, changes: List[Tuple[str, str, Optional[str]]],
                  before: Dict[str, dict], journal_path: str,
                  workers: int) -> Tuple[int, int]:
    def run(entry) -> Optional[str]:
        """Return an error string, or None on success. Threads share no counters."""
        mission_id, project, track = entry
        try:
            api.set_project(mission_id, project, track)
        except Exception as exc:  # noqa: BLE001 - one bad row must not stop the run
            return "%s: %s" % (mission_id[:8], exc)
        return None

    ok = 0
    failed = 0
    with open(journal_path, "a") as journal:
        for batch_start in range(0, len(changes), workers):
            batch = changes[batch_start:batch_start + workers]
            # Journal before the write: a crash mid-batch then over-reports at
            # worst, which `--undo` tolerates (it re-sets values that are
            # already correct). The reverse order would lose changes outright.
            for mission_id, project, track in batch:
                previous = before.get(mission_id, {})
                journal.write(json.dumps({
                    "id": mission_id,
                    "from": {"project": previous.get("project"),
                             "track": previous.get("track")},
                    "to": {"project": project, "track": track},
                }) + "\n")
            journal.flush()
            os.fsync(journal.fileno())
            with ThreadPoolExecutor(max_workers=workers) as pool:
                for error in pool.map(run, batch):
                    if error is None:
                        ok += 1
                    else:
                        failed += 1
                        print("  FAIL " + error, file=sys.stderr)
            time.sleep(0.05)
    return ok, failed


def undo(api: Api, journal_path: str, workers: int) -> None:
    entries = []
    with open(journal_path) as handle:
        for line in handle:
            line = line.strip()
            if line:
                entries.append(json.loads(line))
    print("undoing %d journalled changes (newest first)" % len(entries))
    for entry in reversed(entries):
        original = entry.get("from") or {}
        try:
            api.set_project(entry["id"], original.get("project"), original.get("track"))
        except Exception as exc:  # noqa: BLE001
            print("  FAIL %s: %s" % (entry["id"][:8], exc), file=sys.stderr)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__,
                                     formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--base", default=os.environ.get("SANDBOXED_BASE",
                                                         "http://127.0.0.1:3000"))
    parser.add_argument("--token", default=os.environ.get("SANDBOXED_TOKEN", ""))
    parser.add_argument("--family", action="append", default=[],
                        help="family to collapse; repeatable. Nothing is inferred.")
    parser.add_argument("--apply", action="store_true",
                        help="actually write; without it this is a dry run")
    parser.add_argument("--journal", default="retag-journal.jsonl")
    parser.add_argument("--undo", metavar="JOURNAL",
                        help="revert a previous run from its journal")
    parser.add_argument("--workers", type=int, default=4)
    args = parser.parse_args()

    if not args.token:
        parser.error("--token or SANDBOXED_TOKEN is required")
    api = Api(args.base, args.token)

    if args.undo:
        undo(api, args.undo, args.workers)
        return 0

    if not args.family:
        counts = inventory(api)
        print("=== distinct project values (%d slugs, %d missions)"
              % (len(counts), sum(counts.values())))
        for slug, count in counts.most_common():
            print("  %6d  %s" % (count, slug))
        print("\n=== candidate families — arbitrate by hand, then pass --family")
        for root, slugs in suggest_families(counts):
            total = sum(counts[s] for s in slugs)
            print("  %-24s %5d missions across %d slugs: %s"
                  % (root, total, len(slugs), ", ".join(slugs)))
        print("\nNothing was changed. Re-run with --family <name> [--apply].")
        return 0

    reject_overlapping(args.family)

    total_ok = total_failed = 0
    for family in args.family:
        for attempt in range(1, MAX_PASSES + 1):
            missions = fetch_family(api, family)
            before = {
                m["id"]: {"project": m.get("project"), "track": m.get("track")}
                for m in missions
            }
            changes = [c for c in (plan_change(m, family) for m in missions) if c]
            print("[%s] pass %d: %d missions in family, %d need retagging"
                  % (family, attempt, len(missions), len(changes)))
            if not changes:
                break
            by_source = Counter(before[c[0]]["project"] for c in changes)
            for slug, count in by_source.most_common(10):
                print("    %6d  %s -> %s" % (count, slug, family))
            if not args.apply:
                print("    (dry run — pass --apply to write)")
                break
            ok, failed = apply_changes(api, changes, before, args.journal, args.workers)
            total_ok += ok
            total_failed += failed
            print("    applied %d, failed %d" % (ok, failed))
            if failed and not ok:
                print("    no progress this pass; stopping to avoid a spin")
                break
        else:
            print("[%s] still not converged after %d passes" % (family, MAX_PASSES))

    if args.apply:
        print("\ntotal applied %d, failed %d — journal: %s"
              % (total_ok, total_failed, args.journal))
        if total_failed:
            return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
